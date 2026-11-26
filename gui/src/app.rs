//! The configurator window: device bar, layer list, keypad editor,
//! macros, timing, and the pending-changes/apply bar.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};

use anyhow::{Context, Result, ensure};
use iced::futures::channel::oneshot;
use iced::widget::{
    Column, button, center, checkbox, column, container, mouse_area, opaque, pick_list, row, rule, scrollable, slider,
    space, stack, text, text_input, tooltip,
};
use iced::{Alignment, Color, Element, Fill, Font, Length, Task, Theme};

use bnk8::config::{
    self, Blob, Changes, Config, FACTORY, KEY_SLOTS, KNOB_SLOTS, Key, Layer, Macro, Names, Slots, StudioIds,
};
use bnk8::presets::{self, Preset};
use bnk8::store::{self, Current, Source};
use bnk8::{actions, protocol, transport};

use crate::assistant::{self, Answer, Login};
use crate::style::{self, BOLD, SEMIBOLD, Tone};
use crate::widgets::{Catalog, action_picker, key_picker, macro_summary, pretty, short, tip};

const SLOT_LABELS: [&str; 6] = ["Tap", "Double tap", "Hold", "Tap then hold", "Turn left", "Turn right"];
const KEY_SIZE: (f32, f32) = (136.0, 116.0);
const KNOB_SIZE: f32 = 116.0;
/// Bindings and lighting cards; wide enough for a label and both pickers.
const EDITOR_WIDTH: f32 = 540.0;
const LABEL_WIDTH: f32 = 140.0;
/// A key without its own colour in `Lighting::key_colors`.
const NO_COLOR: &str = "none";

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Control {
    Key(usize),
    Knob,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tab {
    Keys,
    Macros,
    Timing,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    serial: String,
    fw: String,
}

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}  ·  fw {}", elide(&self.serial, 20), self.fw)
    }
}

/// What the app knows about the connected device.
struct Connected {
    serial: String,
    /// The device's config, when bnk8 can prove it (CRC match with the
    /// snapshot, the factory config or the profile store's copy).
    config: Option<Box<Blob>>,
    /// The CRC of the device's config when it was loaded or last written: the
    /// base the shown changes were worked out against.
    crc: u32,
    source: String,
    /// The device has a profile store slot, so writes keep layer and macro names.
    store: bool,
    /// The loaded layer and macro names (profile store only).
    names: Names,
}

/// The Macros tab's "Ask Claude" panel.
struct Ai {
    login: Login,
    prompt: String,
    asking: bool,
    answer: Option<Result<Answer, String>>,
}

/// What reading the device found.
#[derive(Clone, Debug)]
pub enum Read {
    /// `cfg` carries the profile store's layer names when `store`.
    Known { blob: Box<Blob>, cfg: Config, source: String, store: bool },
    Unknown { crc: u32 },
}

/// Results of background device work. Every hidapi call runs on one
/// long-lived worker thread: on macOS hidapi pumps the calling thread's
/// run loop (aborting winit on the main thread) and schedules its global
/// HID manager on the first caller's run loop (so that thread must live).
#[derive(Clone, Debug)]
pub enum Done {
    Devices(Vec<Device>),
    Loaded { serial: String, read: Read },
    /// `edits` is the editor's config when the write started; `blob` is what
    /// was written (`None` when the device already had it).
    Written { blob: Option<Box<Blob>>, changes: usize, edits: Config, store: bool },
}

type Job = Box<dyn FnOnce() + Send>;

/// Validation and diff of the edited config, recomputed when it changes.
enum Preview {
    Invalid(String),
    Ready { changes: Changes },
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Verb {
    Tap,
    Press,
    Release,
    Delay,
}

impl Verb {
    const ALL: [Verb; 4] = [Verb::Tap, Verb::Press, Verb::Release, Verb::Delay];

    fn parse(s: &str) -> Option<Verb> {
        Some(match s {
            "tap" => Verb::Tap,
            "press" => Verb::Press,
            "release" => Verb::Release,
            "delay" => Verb::Delay,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Verb::Tap => "tap",
            Verb::Press => "press",
            Verb::Release => "release",
            Verb::Delay => "delay",
        }
    }
}

impl fmt::Display for Verb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Verb::Tap => "Tap",
            Verb::Press => "Press",
            Verb::Release => "Release",
            Verb::Delay => "Delay",
        })
    }
}

/// A macro step split into its verb (`None` for raw bytes) and argument.
fn parse_step(step: &str) -> (Option<Verb>, &str) {
    if step.starts_with("raw.") {
        return (None, step);
    }
    let (verb, arg) = step.split_once(' ').unwrap_or((step, ""));
    match Verb::parse(verb) {
        Some(v) => (Some(v), arg),
        None => (None, step),
    }
}

#[derive(Clone, Debug)]
pub enum LightingEdit {
    Effect(u8),
    Brightness(u8),
    Speed(u8),
    Color(String),
    Channel(usize, u8),
    Shared(bool),
    /// Key index 0..8 and its colour text (`#RRGGBB`; empty = none).
    KeyColor(usize, String),
}

/// An effect in the effect picker, shown by its name.
#[derive(Clone, Copy, PartialEq, Debug)]
struct EffectChoice(u8);

impl fmt::Display for EffectChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match config::effect_name(self.0) {
            Some(name) => f.write_str(&pretty(name)),
            None => write!(f, "Effect {}", self.0),
        }
    }
}

const EFFECT_CHOICES: [EffectChoice; config::EFFECTS.len()] = {
    let mut out = [EffectChoice(0); config::EFFECTS.len()];
    let mut i = 0;
    while i < out.len() {
        out[i] = EffectChoice(config::EFFECTS[i].0);
        i += 1;
    }
    out
};

#[derive(Clone, Debug)]
pub enum StepEdit {
    Verb(Verb),
    Key(String),
    Delay(u16),
    Raw(String),
    Up,
    Down,
    Remove,
}

#[derive(Clone, Debug)]
pub enum AiEvent {
    Check,
    Checked(Result<Login, String>),
    SignIn,
    Prompt(String),
    Ask,
    Answered(Result<Answer, String>),
    Accept,
    Dismiss,
}

#[derive(Clone, Debug)]
pub enum Message {
    Finished(Result<Done, String>),
    RefreshDevices,
    SelectDevice(Device),
    Load,
    DismissBanner,
    SelectTab(Tab),
    SelectLayer(usize),
    AddLayer,
    AddPreset(&'static Preset),
    RemoveLayer,
    SelectControl(Control),
    SetSlot(&'static str, Option<String>),
    Lighting(LightingEdit),
    SelectMacro(usize),
    MacroName(String),
    Step(usize, StepEdit),
    AddKeyStep,
    AddDelay,
    ClearSteps,
    DoubleTap(u16),
    Hold(u16),
    ToggleChanges,
    Apply,
    Discard,
    Import,
    Imported(Option<PathBuf>),
    Export,
    Exported(Option<PathBuf>),
    AskReset,
    ConfirmReset,
    CancelReset,
    Ai(AiEvent),
}

pub struct App {
    devices: Vec<Device>,
    serial: Option<String>,
    connected: Option<Connected>,
    cfg: Config,
    layer: usize,
    control: Control,
    tab: Tab,
    macro_slot: usize,
    preview: Preview,
    preview_of: Option<Config>,
    /// Label of the running device job, if any.
    busy: Option<&'static str>,
    worker: Sender<Job>,
    message: Option<(Tone, String)>,
    show_changes: bool,
    /// The device a factory reset was asked for, while the dialog is open.
    confirm_reset: Option<String>,
    catalog: Catalog,
    /// Layer templates, as pick-list options.
    templates: Vec<&'static Preset>,
    ai: Ai,
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        let (worker, jobs) = channel::<Job>();
        std::thread::Builder::new()
            .name("hid".into())
            .spawn(move || {
                for job in jobs {
                    job();
                }
            })
            .expect("spawning the HID thread");
        let mut app = App {
            devices: Vec::new(),
            serial: None,
            connected: None,
            cfg: config::from_blob(FACTORY).expect("factory config decodes"),
            layer: 0,
            control: Control::Key(0),
            tab: Tab::Keys,
            macro_slot: 0,
            preview: Preview::Invalid(String::new()),
            preview_of: None,
            busy: None,
            worker,
            message: None,
            show_changes: false,
            confirm_reset: None,
            catalog: Catalog::new(),
            templates: presets::PRESETS.iter().collect(),
            ai: Ai { login: Login::Unchecked, prompt: String::new(), asking: false, answer: None },
        };
        let task = app.refresh_devices();
        app.refresh_preview();
        (app, task)
    }

    fn info(&mut self, text: impl Into<String>) {
        self.message = Some((Tone::Success, text.into()));
    }

    fn error(&mut self, text: impl Into<String>) {
        self.message = Some((Tone::Danger, text.into()));
    }

    fn spawn(&mut self, label: &'static str, f: impl FnOnce() -> Result<Done> + Send + 'static) -> Task<Message> {
        let (tx, rx) = oneshot::channel();
        let job: Job = Box::new(move || {
            let _ = tx.send(f().map_err(|e| format!("{e:#}")));
        });
        if self.worker.send(job).is_err() {
            self.error("The HID worker thread stopped; restart the app.");
            return Task::none();
        }
        self.busy = Some(label);
        Task::perform(rx, |r| {
            Message::Finished(r.unwrap_or_else(|_| Err("The HID worker thread stopped; restart the app.".into())))
        })
    }

    fn refresh_devices(&mut self) -> Task<Message> {
        self.spawn("Looking for devices", || {
            let api = hidapi::HidApi::new()?;
            Ok(Done::Devices(
                transport::find(&api)
                    .iter()
                    .filter_map(|d| {
                        Some(Device {
                            serial: d.serial_number()?.to_string(),
                            fw: transport::format_fw(d.release_number()),
                        })
                    })
                    .collect(),
            ))
        })
    }

    fn load(&mut self) -> Task<Message> {
        let serial = self.serial.clone();
        self.spawn("Reading device", move || {
            let dev = transport::open(serial.as_deref())?;
            let read = match store::current(&dev)? {
                Current::Known(blob, source) => {
                    // Decoded here, so a config bnk8 can't model is an error, not a panic.
                    let cfg = store::config(&blob, &source).context("decoding the device's config")?;
                    let store = matches!(source, Source::ProfileStore { .. });
                    let source = match source {
                        Source::Factory => "factory default".into(),
                        Source::OlderFactory(version) => format!("factory default (firmware {version})"),
                        Source::Snapshot => "last written by bnk8".into(),
                        Source::ProfileStore { .. } => "the device's profile store".into(),
                    };
                    Read::Known { blob, cfg, source, store }
                }
                Current::Unknown { crc } => Read::Unknown { crc },
            };
            Ok(Done::Loaded { serial: dev.serial().to_string(), read })
        })
    }

    /// Writes `cfg`, through the device's profile store when it has one. With
    /// `same_base`, refuses when the device's config is no longer the one the
    /// shown changes were worked out against.
    fn write(&mut self, cfg: Config, same_base: bool) -> Task<Message> {
        let Some((serial, crc)) = self.target().map(|c| (c.serial.clone(), c.crc)) else { return Task::none() };
        self.spawn("Writing to device", move || {
            let dev = transport::open(Some(&serial))?;
            let p = store::prepare(&dev, &cfg)?;
            ensure!(
                !same_base || p.device_crc == crc,
                "The keypad's config changed since it was loaded, so the changes shown are out of date. \
                 Press Load from device, check the changes, then apply again."
            );
            let changes = match &p.changes {
                Changes::Items(lines) => lines.len(),
                Changes::Unknown => config::flatten(&cfg).len(),
            };
            let blob = if p.unchanged() {
                None
            } else {
                store::commit(&dev, &p)?;
                Some(Box::new(p.native))
            };
            Ok(Done::Written { blob, changes, edits: cfg, store: p.saves_to_profile_store() })
        })
    }

    fn finished(&mut self, result: Result<Done, String>) -> Task<Message> {
        self.busy = None;
        // The diff base may have changed even if the edited config did not.
        self.preview_of = None;
        match result {
            Err(e) => self.error(e),
            Ok(Done::Devices(devices)) => {
                self.devices = devices;
                if self.serial.as_ref().is_none_or(|s| !self.devices.iter().any(|d| &d.serial == s)) {
                    self.serial = self.devices.first().map(|d| d.serial.clone());
                }
                if self.devices.is_empty() {
                    self.error("No BNK8 AI Edition found. Plug it in and press Refresh.");
                } else if self.connected.is_none() {
                    return self.load();
                }
            }
            Ok(Done::Loaded { serial, read }) => {
                let (blob, cfg, source, store, crc) = match read {
                    Read::Known { blob, cfg, source, store } => {
                        self.info(format!("Loaded {serial} ({source})"));
                        let crc = protocol::crc32(&*blob);
                        (Some(blob), cfg, source, store, crc)
                    }
                    Read::Unknown { crc } => {
                        self.error(format!(
                            "This device's config (CRC {crc:#010x}) can't be identified: it isn't bnk8's last write, \
                             the factory config, or the copy saved on the device. Editing starts from the \
                             factory defaults; Apply writes the whole config."
                        ));
                        let cfg = config::from_blob(FACTORY).expect("factory config decodes");
                        (None, cfg, "unknown".into(), false, crc)
                    }
                };
                let names = Names::of(&cfg);
                self.cfg = cfg;
                self.clamp_layer();
                self.connected = Some(Connected { serial, config: blob, crc, source, store, names });
            }
            Ok(Done::Written { blob, changes, edits, store }) => {
                let Some(blob) = blob else {
                    self.info("The device already has this config");
                    return Task::none();
                };
                // Names written: the edits' own, else the loaded ones (prepare keeps those).
                let names = match &self.connected {
                    Some(c) if store => {
                        let mut named = edits.clone();
                        c.names.fill(&mut named);
                        Names::of(&named)
                    }
                    _ => Names::default(),
                };
                // The written config is the new diff base. Edits made while it was
                // being written stay pending against it; otherwise the editor
                // shows what the device now holds (the factory config after a reset).
                if self.cfg == edits {
                    let mut cfg = config::from_blob(&blob).expect("written blob decodes");
                    names.fill(&mut cfg);
                    self.cfg = cfg;
                    self.clamp_layer();
                }
                if let Some(c) = &mut self.connected {
                    c.crc = protocol::crc32(&*blob);
                    c.config = Some(blob);
                    c.source = if store { "the device's profile store" } else { "last written by bnk8" }.into();
                    c.store = store;
                    c.names = names;
                }
                self.info(format!("Saved {changes} change(s) to the device"));
            }
        }
        Task::none()
    }

    fn base(&self) -> &Blob {
        self.connected.as_ref().and_then(|c| c.config.as_deref()).unwrap_or(FACTORY)
    }

    /// `Layer N`, plus its name when it has one.
    fn layer_title(&self, i: usize) -> String {
        match self.cfg.layers.get(i).and_then(|l| l.name.as_deref()) {
            Some(name) => format!("Layer {i} · {name}"),
            None => format!("Layer {i}"),
        }
    }

    /// The connected device, when it is also the one selected in the device
    /// picker: Apply and Factory reset write only to a device whose config is loaded.
    fn target(&self) -> Option<&Connected> {
        self.connected.as_ref().filter(|c| self.serial.as_deref() == Some(c.serial.as_str()))
    }

    /// What writing `blob` (the edited config) changes, per item; on devices
    /// with a profile store, layer and macro names count too (unnamed ones keep
    /// their name).
    fn changes(&self, blob: &Blob) -> Result<Changes> {
        let Some(c) = self.connected.as_ref() else { return config::changes(None, blob) };
        let Some(before) = c.config.as_deref() else { return config::changes(None, blob) };
        if !c.store {
            return config::changes(Some(before), blob);
        }
        let named = |b: &Blob, names: &[&Names]| -> Result<Config> {
            let mut cfg = config::from_blob(b)?;
            names.iter().for_each(|n| n.fill(&mut cfg));
            Ok(cfg)
        };
        let mut lines = config::diff(&named(before, &[&c.names])?, &named(blob, &[&Names::of(&self.cfg), &c.names])?);
        if lines.is_empty() && blob != before {
            lines.push("action table: reordered, bindings unchanged".into());
        }
        Ok(Changes::Items(lines))
    }

    fn refresh_preview(&mut self) {
        if self.preview_of.as_ref() == Some(&self.cfg) {
            return;
        }
        let result = config::to_blob(&self.cfg, self.base(), &StudioIds::new()).and_then(|b| self.changes(&b));
        self.preview = match result {
            Ok(changes) => Preview::Ready { changes },
            Err(e) => Preview::Invalid(format!("{e:#}")),
        };
        self.preview_of = Some(self.cfg.clone());
    }

    /// Number of changes Apply would write, when it can write.
    fn pending(&self) -> Option<usize> {
        let Preview::Ready { changes } = &self.preview else { return None };
        Some(match changes {
            Changes::Unknown => config::flatten(&self.cfg).len(),
            Changes::Items(lines) => lines.len(),
        })
    }

    fn clamp_layer(&mut self) {
        self.layer = self.layer.min(self.cfg.layers.len() - 1);
    }

    // ---- update -------------------------------------------------------

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let task = self.apply_message(message);
        self.refresh_preview();
        task
    }

    fn apply_message(&mut self, message: Message) -> Task<Message> {
        let idle = self.busy.is_none();
        match message {
            Message::Finished(result) => return self.finished(result),
            Message::Ai(e) => return self.ai_event(e),
            Message::RefreshDevices if idle => return self.refresh_devices(),
            Message::SelectDevice(d) => self.serial = Some(d.serial),
            Message::Load if idle && self.serial.is_some() => return self.load(),
            Message::DismissBanner => self.message = None,
            Message::SelectTab(tab) => {
                self.tab = tab;
                // Looked up lazily: users who never open Macros never run `claude`.
                if tab == Tab::Macros && matches!(self.ai.login, Login::Unchecked) {
                    return self.ai_event(AiEvent::Check);
                }
            }
            Message::SelectLayer(i) if i < self.cfg.layers.len() => {
                self.layer = i;
                self.tab = Tab::Keys;
            }
            Message::AddLayer if self.cfg.layers.len() < actions::MAX_LAYERS => {
                let knob = Slots { hold: Some("layer.next".into()), ..Slots::default() };
                let lighting = self.cfg.layers[self.layer].lighting.clone();
                let layer = Layer { name: None, keys: vec![Key::Tap("none".into()); config::KEYS], knob, lighting };
                self.cfg.layers.push(layer);
                self.layer = self.cfg.layers.len() - 1;
            }
            Message::AddPreset(preset) => {
                let lighting = self.cfg.layers[self.layer].lighting.clone();
                match presets::add(&mut self.cfg, preset, &lighting) {
                    Ok(i) => {
                        self.layer = i;
                        self.tab = Tab::Keys;
                        self.control = Control::Key(0);
                        let legend: Vec<String> =
                            preset.legend.iter().enumerate().map(|(k, l)| format!("K{} {l}", k + 1)).collect();
                        self.info(format!("Added {} as layer {i}: {}. Knob: volume, press mutes the mic.", preset.name, legend.join(" · ")));
                    }
                    Err(e) => self.error(format!("{e:#}")),
                }
            }
            Message::RemoveLayer if self.cfg.layers.len() > 1 => {
                let removed = self.layer;
                self.cfg.layers.remove(removed);
                let cleared = renumber_layer_refs(&mut self.cfg, removed);
                self.clamp_layer();
                if cleared > 0 {
                    self.info(format!("Removed layer {removed}; cleared {cleared} binding(s) that switched to it."));
                }
            }
            Message::SelectControl(c) => self.control = c,
            Message::SetSlot(name, value) => {
                let layer = &mut self.cfg.layers[self.layer];
                match self.control {
                    Control::Key(k) => {
                        let mut slots = layer.keys[k].slots();
                        let slot = slots.slot_mut(name).expect("key slot name");
                        if *slot != value {
                            *slot = value;
                            layer.keys[k] = Key::from_slots(slots);
                        }
                    }
                    Control::Knob => *layer.knob.slot_mut(name).expect("knob slot name") = value,
                }
            }
            Message::Lighting(edit) => {
                let p = self.layer;
                let l = &mut self.cfg.layers[p].lighting;
                match edit {
                    LightingEdit::Effect(v) => l.effect = v,
                    LightingEdit::Brightness(v) => l.brightness = v,
                    LightingEdit::Speed(v) => l.speed = v,
                    LightingEdit::Color(c) => l.color = c,
                    LightingEdit::Channel(i, v) => {
                        let mut rgb = config::parse_color(&l.color).unwrap_or([255, 255, 255]);
                        rgb[i] = v;
                        l.color = config::rgb_hex(&rgb);
                    }
                    LightingEdit::Shared(on) => l.shared = on,
                    LightingEdit::KeyColor(k, s) => {
                        // Key colours overwrite the bytes `extra` holds.
                        l.extra = None;
                        let colors = l.key_colors.get_or_insert_with(|| vec![NO_COLOR.into(); config::KEYS]);
                        if colors.len() <= k {
                            colors.resize(k + 1, NO_COLOR.into());
                        }
                        let s = s.trim();
                        colors[k] = if s.is_empty() { NO_COLOR.into() } else { s.to_string() };
                        if colors.iter().all(|c| c == NO_COLOR) {
                            l.key_colors = None;
                        }
                    }
                }
                // Studio's flags byte follows from the settings.
                l.flags = None;
                // Shared lighting is one setup copied to every layer.
                let l = l.clone();
                for layer in &mut self.cfg.layers {
                    if l.shared {
                        layer.lighting = l.clone();
                    } else {
                        layer.lighting.shared = false;
                    }
                }
            }
            Message::SelectMacro(m) => self.macro_slot = m,
            Message::MacroName(s) => {
                let slot = self.macro_slot;
                let key = config::macro_key(slot);
                let loaded = self.connected.as_ref().is_some_and(|c| c.names.macros.contains_key(&key));
                if let Some(m) = self.cfg.macros.get_mut(&key) {
                    let s: String = s.chars().filter(|c| !c.is_control()).take(config::MAX_NAME).collect();
                    // Cleared: the default name, as `None` would keep the loaded one.
                    m.name = match s.trim() {
                        "" if loaded => Some(config::default_macro_name(slot)),
                        "" => None,
                        _ => Some(s),
                    };
                }
            }
            Message::Step(i, edit) => self.edit_macro(|steps| {
                // A stale message for a step that no longer exists.
                if i >= steps.len() {
                    return;
                }
                match edit {
                    StepEdit::Verb(v) => {
                        let (old, arg) = parse_step(&steps[i]);
                        if old == Some(v) {
                            return;
                        }
                        steps[i] = match (v, old) {
                            (Verb::Delay, _) => "delay 100".into(),
                            (v, Some(Verb::Tap | Verb::Press | Verb::Release)) => format!("{} {arg}", v.name()),
                            (v, _) => format!("{} keyboard.a", v.name()),
                        };
                    }
                    StepEdit::Key(k) => {
                        if let (Some(v), _) = parse_step(&steps[i]) {
                            steps[i] = format!("{} {k}", v.name());
                        }
                    }
                    StepEdit::Delay(ms) => steps[i] = format!("delay {}", ms / 10 * 10),
                    StepEdit::Raw(s) => steps[i] = s,
                    StepEdit::Up if i > 0 => steps.swap(i - 1, i),
                    StepEdit::Down if i + 1 < steps.len() => steps.swap(i, i + 1),
                    StepEdit::Remove => {
                        steps.remove(i);
                    }
                    StepEdit::Up | StepEdit::Down => {}
                }
            }),
            Message::AddKeyStep => self.edit_macro(|steps| {
                if key_steps(steps) < config::MACRO_STEPS {
                    steps.push("tap keyboard.a".into());
                }
            }),
            Message::AddDelay => self.edit_macro(|steps| steps.push("delay 100".into())),
            Message::ClearSteps => self.edit_macro(Vec::clear),
            Message::DoubleTap(ms) => self.cfg.timing.double_tap_ms = ms,
            Message::Hold(ms) => self.cfg.timing.hold_ms = ms,
            Message::ToggleChanges => self.show_changes = !self.show_changes,
            Message::Apply if idle && self.target().is_some() => {
                if self.pending().is_some_and(|n| n > 0) {
                    return self.write(self.cfg.clone(), true);
                }
            }
            Message::Discard if idle => {
                let mut cfg = config::from_blob(self.base()).expect("base decodes");
                if let Some(c) = &self.connected {
                    c.names.fill(&mut cfg);
                }
                self.cfg = cfg;
                self.clamp_layer();
            }
            Message::Import => {
                return Task::perform(
                    rfd::AsyncFileDialog::new().add_filter("TOML", &["toml"]).pick_file(),
                    |f| Message::Imported(f.map(|f| f.path().to_path_buf())),
                );
            }
            Message::Imported(Some(path)) => self.import(path),
            Message::Export => {
                return Task::perform(
                    rfd::AsyncFileDialog::new().add_filter("TOML", &["toml"]).set_file_name("bnk8.toml").save_file(),
                    |f| Message::Exported(f.map(|f| f.path().to_path_buf())),
                );
            }
            Message::Exported(Some(path)) => self.export(path),
            Message::AskReset if idle => self.confirm_reset = self.target().map(|c| c.serial.clone()),
            Message::ConfirmReset => {
                // Only for the device the dialog was opened for, and only while it is still the target.
                let asked = self.confirm_reset.take();
                if idle && asked.is_some() && asked.as_deref() == self.target().map(|c| c.serial.as_str()) {
                    return self.write(config::from_blob(FACTORY).expect("factory config decodes"), false);
                }
            }
            Message::CancelReset => self.confirm_reset = None,
            // Stale clicks on controls that are disabled now, and cancelled dialogs.
            Message::RefreshDevices
            | Message::Load
            | Message::SelectLayer(_)
            | Message::AddLayer
            | Message::RemoveLayer
            | Message::Apply
            | Message::Discard
            | Message::AskReset
            | Message::Imported(None)
            | Message::Exported(None) => {}
        }
        Task::none()
    }

    /// Edits the selected macro's steps; an emptied macro is removed.
    fn edit_macro(&mut self, f: impl FnOnce(&mut Vec<String>)) {
        let key = config::macro_key(self.macro_slot);
        let mut m = self.cfg.macros.remove(&key).unwrap_or_default();
        f(&mut m.steps);
        if !m.steps.is_empty() {
            self.cfg.macros.insert(key, m);
        }
    }

    /// Macro `slot`'s name as Apply writes it (its own, else the loaded one),
    /// unless it is the default `Macro N`.
    fn macro_name(&self, slot: usize) -> Option<&str> {
        let key = config::macro_key(slot);
        let own = self.cfg.macros.get(&key)?.name.as_ref();
        let loaded = self.connected.as_ref().and_then(|c| c.names.macros.get(&key));
        own.or(loaded).map(String::as_str).filter(|n| *n != config::default_macro_name(slot))
    }

    /// The "Ask Claude" panel. Claude Code runs on threads of its own, never
    /// the HID worker: its calls take seconds.
    fn ai_event(&mut self, e: AiEvent) -> Task<Message> {
        match e {
            // Allowed while signing in, so a sign-in finished in Terminal shows up.
            AiEvent::Check if !matches!(self.ai.login, Login::Checking) => {
                self.ai.login = Login::Checking;
                return Task::perform(assistant::background(assistant::check), |r| Message::Ai(AiEvent::Checked(r)));
            }
            AiEvent::Check => {}
            AiEvent::Checked(r) => self.ai.login = r.unwrap_or_else(Login::Failed),
            AiEvent::SignIn => {
                if let Login::SignedOut(path) = &self.ai.login {
                    let path = path.clone();
                    self.ai.login = Login::SigningIn;
                    return Task::perform(assistant::background(move || assistant::sign_in(&path)), |r| {
                        Message::Ai(AiEvent::Checked(r))
                    });
                }
            }
            AiEvent::Prompt(s) => self.ai.prompt = s,
            AiEvent::Ask => {
                let prompt = self.ai.prompt.trim().to_owned();
                if let Login::SignedIn { claude, .. } = &self.ai.login
                    && !self.ai.asking
                    && !prompt.is_empty()
                {
                    let claude = claude.clone();
                    let mut named = self.cfg.clone();
                    if let Some(c) = &self.connected {
                        c.names.fill(&mut named);
                    }
                    let system = assistant::system_prompt(&named.macros, self.macro_slot);
                    self.ai.asking = true;
                    self.ai.answer = None;
                    return Task::perform(assistant::background(move || assistant::ask(&claude, &system, &prompt)), |r| {
                        Message::Ai(AiEvent::Answered(r))
                    });
                }
            }
            AiEvent::Answered(r) => {
                self.ai.asking = false;
                self.ai.answer = Some(r);
            }
            AiEvent::Accept => {
                if let Some(Ok(a)) = &self.ai.answer
                    && !a.macros.is_empty()
                {
                    for p in &a.macros {
                        let m = Macro { name: Some(p.name.clone()), steps: p.steps.clone() };
                        self.cfg.macros.insert(config::macro_key(p.slot), m);
                    }
                    self.macro_slot = a.macros[0].slot;
                    let list = a
                        .macros
                        .iter()
                        .map(|p| format!("Macro {} ({})", p.slot + 1, p.name))
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.ai.answer = None;
                    self.info(format!(
                        "Added {list} from Claude (not yet applied). Bind it to a key on Keys & lighting, then Apply to device."
                    ));
                }
            }
            AiEvent::Dismiss => self.ai.answer = None,
        }
        Task::none()
    }

    fn import(&mut self, path: PathBuf) {
        let result = std::fs::read_to_string(&path)
            .context("reading")
            .and_then(|t| config::from_toml(&t))
            .and_then(|c| config::to_blob(&c, FACTORY, &StudioIds::new()).map(|_| c));
        match result {
            Ok(c) => {
                self.cfg = c;
                self.clamp_layer();
                self.info(format!("Imported {} (not yet applied)", path.display()));
            }
            Err(e) => self.error(format!("{}: {e:#}", path.display())),
        }
    }

    fn export(&mut self, path: PathBuf) {
        match config::to_toml(&self.cfg).and_then(|t| std::fs::write(&path, t).context("writing")) {
            Ok(()) => self.info(format!("Exported {}", path.display())),
            Err(e) => self.error(format!("{}: {e:#}", path.display())),
        }
    }

    // ---- view ---------------------------------------------------------

    pub fn view(&self) -> Element<'_, Message> {
        let body = row![
            container(self.sidebar()).width(220).height(Fill).style(style::chrome),
            rule::vertical(1).style(rule::weak),
            scrollable(container(self.content()).padding(28).width(Fill)).height(Fill).width(Fill),
        ];
        let page = column![
            container(self.header()).width(Fill).style(style::chrome),
            rule::horizontal(1).style(rule::weak),
        ]
        .push(self.banner())
        .push(body.height(Fill))
        .push(rule::horizontal(1).style(rule::weak))
        .push(container(self.footer()).width(Fill).style(style::chrome));
        if self.confirm_reset.is_some() { stack![page, self.reset_dialog()].into() } else { page.into() }
    }

    fn header(&self) -> Element<'_, Message> {
        let idle = self.busy.is_none();
        let brand = row![
            container(text("B8").size(14).font(BOLD)).padding([6, 8]).style(style::pill(Tone::Accent)),
            column![
                text("BNK8 AI Edition").size(17).font(BOLD),
                text("Configurator").size(12).style(style::muted)
            ],
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        let status: Element<'_, Message> = match (&self.busy, &self.connected) {
            (Some(label), _) => status_pill(Tone::Accent, format!("{label}…")),
            (None, Some(c)) if c.config.is_some() => status_pill(Tone::Success, format!("Config: {}", c.source)),
            (None, Some(c)) => status_pill(Tone::Danger, format!("Config: {}", c.source)),
            (None, None) => status_pill(Tone::Neutral, "Not connected".into()),
        };
        let selected = self.devices.iter().find(|d| Some(&d.serial) == self.serial.as_ref());
        row![
            brand,
            space::horizontal(),
            status,
            pick_list(&self.devices[..], selected, Message::SelectDevice)
                .placeholder("No device found")
                .width(310)
                .text_size(13)
                .style(style::field),
            button("Refresh").style(style::ghost).on_press_maybe(idle.then_some(Message::RefreshDevices)),
            button("Load from device")
                .style(style::ghost)
                .on_press_maybe((idle && self.serial.is_some()).then_some(Message::Load)),
        ]
        .spacing(10)
        .padding([12, 20])
        .align_y(Alignment::Center)
        .into()
    }

    fn banner(&self) -> Option<Element<'_, Message>> {
        let (tone, msg) = self.message.as_ref()?;
        let content = row![
            container(space()).width(8).height(8).style(style::dot(*tone)),
            text(msg).size(13).width(Fill),
            button(text("✕").size(12)).style(style::icon).padding([2, 8]).on_press(Message::DismissBanner),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        Some(container(container(content).padding([8, 12]).style(style::banner(*tone))).padding([10, 20]).into())
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let nav = [(Tab::Keys, "Keys & lighting"), (Tab::Macros, "Macros"), (Tab::Timing, "Timing")]
            .into_iter()
            .map(|(tab, label)| nav_button(text(label).size(14), self.tab == tab, Message::SelectTab(tab)));
        let layers = self.cfg.layers.iter().enumerate().map(|(i, layer)| {
            let content = row![
                container(space()).width(12).height(12).style(style::swatch(layer_color(layer))),
                text(self.layer_title(i)).size(14),
            ]
            .spacing(10)
            .align_y(Alignment::Center);
            nav_button(content, self.layer == i, Message::SelectLayer(i))
        });
        let n = self.cfg.layers.len();
        column![
            section_label("VIEW"),
            Column::with_children(nav).spacing(2),
            space().height(18),
            section_label("LAYERS"),
            text("Hold the knob to switch layers (factory binding).").size(11).style(style::muted),
            space().height(4),
            Column::with_children(layers).spacing(2),
            space().height(6),
            row![
                button(text("+ Add").size(13))
                    .style(style::ghost)
                    .on_press_maybe((n < actions::MAX_LAYERS).then_some(Message::AddLayer)),
                tooltip(
                    button(text("Remove").size(13))
                        .style(style::ghost_danger)
                        .on_press_maybe((n > 1).then_some(Message::RemoveLayer)),
                    tip("Remove the selected layer"),
                    tooltip::Position::Bottom,
                ),
            ]
            .spacing(8),
            tooltip(
                pick_list(&self.templates[..], None::<&Preset>, Message::AddPreset)
                    .placeholder("+ Add from template…")
                    .width(Fill)
                    .text_size(13)
                    .style(style::field),
                tip("Adds a new layer of meeting shortcuts. Shortcuts with modifiers use free macro slots."),
                tooltip::Position::Bottom,
            ),
        ]
        .spacing(6)
        .padding(16)
        .into()
    }

    fn content(&self) -> Element<'_, Message> {
        match self.tab {
            Tab::Keys => self.keys_tab(),
            Tab::Macros => self.macros_tab(),
            Tab::Timing => self.timing_tab(),
        }
    }

    fn keys_tab(&self) -> Element<'_, Message> {
        let l = self.layer;
        column![
            page_title(self.layer_title(l), "Pick a key or the knob, then choose what each gesture does."),
            // Wraps the editors below the keypad when the window is too narrow for both.
            row![
                column![
                    self.keypad(),
                    text("Layout as seen from above; the knob is top right.").size(11).style(style::muted),
                    text("+N: extra gestures bound. L/R: knob turns.").size(11).style(style::muted),
                ]
                .spacing(8),
                column![self.slot_editor(), self.lighting_editor()].spacing(18).width(EDITOR_WIDTH),
            ]
            .spacing(28)
            .wrap()
            .vertical_spacing(22),
        ]
        .spacing(22)
        .into()
    }

    fn keypad(&self) -> Element<'_, Message> {
        let layer = &self.cfg.layers[self.layer];
        let rows: [[Control; 3]; 3] = [
            [Control::Key(0), Control::Key(1), Control::Knob],
            [Control::Key(2), Control::Key(3), Control::Key(4)],
            [Control::Key(5), Control::Key(6), Control::Key(7)],
        ];
        let grid = Column::with_children(rows.map(|r| {
            row(r.map(|c| self.keycap(layer, c))).spacing(12).into()
        }))
        .spacing(12);
        let glow = (layer.lighting.effect != 0).then(|| layer_color(layer));
        container(grid).padding(20).style(style::device(glow)).into()
    }

    fn keycap(&self, layer: &Layer, c: Control) -> Element<'_, Message> {
        let selected = self.control == c;
        let (title, slots) = match c {
            Control::Key(k) => (format!("K{}", k + 1), layer.keys[k].slots()),
            Control::Knob => ("Knob".to_string(), layer.knob.clone()),
        };
        // A key's face shows its tap binding, or its hold binding when tap is empty.
        let (face, on_hold) = match (&slots.tap, &slots.hold) {
            (None, Some(hold)) if c != Control::Knob => (Some(hold.as_str()), true),
            (tap, _) => (tap.as_deref(), false),
        };
        let bound = face.is_some();
        let gestures = KEY_SLOTS.iter().filter(|s| slots.get(s).is_some()).count();
        let extra = gestures - usize::from(bound);
        // Macros read as their name, else the shortcut they play (Cmd+Shift+A).
        let shortcut = face
            .and_then(|f| f.strip_prefix("macro."))
            .and_then(|n| {
                let m = self.cfg.macros.get(n)?;
                let name = self.macro_name(n.parse::<usize>().ok()?.checked_sub(1)?);
                Some(name.map_or_else(|| macro_summary(&m.steps), str::to_string))
            })
            .filter(|s| !s.is_empty());
        let label = shortcut.clone().unwrap_or_else(|| short(face));
        let note = match (shortcut.is_some().then(|| short(face)), on_hold) {
            (Some(name), true) => Some(format!("{name} · hold")),
            (Some(name), false) => Some(name),
            (None, true) => Some("hold".to_string()),
            (None, false) => None,
        };
        let main = |max: usize, size: f32| {
            text(elide(&label, max))
                .size(size)
                .font(SEMIBOLD)
                .style(move |t: &Theme| if bound { text::base(t) } else { style::muted(t) })
        };
        let badge = (extra > 0).then(|| text(format!("+{extra}")).size(11).style(style::accent));
        match c {
            Control::Key(k) => {
                let key_color = layer.lighting.key_colors.as_ref().and_then(|cs| config::parse_color(cs.get(k)?));
                let swatch = key_color.map(|[r, g, b]| {
                    container(space()).width(10).height(10).style(style::swatch(Color::from_rgb8(r, g, b)))
                });
                let content = column![
                    row![text(title).size(12).style(style::muted)]
                        .push(swatch)
                        .push(space::horizontal())
                        .push(badge)
                        .spacing(6)
                        .align_y(Alignment::Center),
                    space::vertical(),
                    main(14, 17.0),
                ]
                .push(note.map(|n| text(n).size(11).style(style::muted)));
                button(content)
                    .width(KEY_SIZE.0)
                    .height(KEY_SIZE.1)
                    .padding(12)
                    .style(style::keycap(selected, 10.0))
                    .on_press(Message::SelectControl(c))
                    .into()
            }
            Control::Knob => {
                let turns = (slots.ccw.is_some() || slots.cw.is_some()).then(|| {
                    let turn = |dir: &str, slot: &Option<String>| {
                        text(format!("{dir}: {}", elide(&short(slot.as_deref()), 9))).size(11).style(style::muted)
                    };
                    column![turn("L", &slots.ccw), turn("R", &slots.cw)].align_x(Alignment::Center)
                });
                let content = column![row![text(title).size(11).style(style::muted)].push(badge).spacing(4), main(12, 14.0)]
                    .push(turns)
                    .spacing(2)
                    .align_x(Alignment::Center);
                let knob = button(center(content))
                    .width(KNOB_SIZE)
                    .height(KNOB_SIZE)
                    .padding(6)
                    .style(style::keycap(selected, KNOB_SIZE / 2.0))
                    .on_press(Message::SelectControl(c));
                container(knob).center_x(KEY_SIZE.0).center_y(KEY_SIZE.1).into()
            }
        }
    }

    fn slot_editor(&self) -> Element<'_, Message> {
        let layer = &self.cfg.layers[self.layer];
        let (title, names, labels): (String, &[&'static str], &[&str]) = match self.control {
            Control::Key(k) => (format!("K{}", k + 1), &KEY_SLOTS, &SLOT_LABELS[..4]),
            Control::Knob => ("Knob".into(), &KNOB_SLOTS, &SLOT_LABELS),
        };
        let knob = self.control == Control::Knob;
        let macro_labels = self.macro_labels();
        let rows = names.iter().zip(labels).map(|(&name, &label)| {
            let label = if knob && name == "tap" { "Press" } else { label };
            let value = match self.control {
                Control::Key(k) => key_slot(&layer.keys[k], name),
                Control::Knob => layer.knob.get(name).map(String::as_str),
            };
            field_row(label, action_picker(&self.catalog, value, &macro_labels, move |v| Message::SetSlot(name, v)))
        });
        card(
            format!("{title} bindings"),
            column![Column::with_children(rows).spacing(10)]
                .push(hint("Tap fires instantly when the other gestures are empty; otherwise after the double-tap window."))
                .spacing(12),
        )
    }

    fn lighting_editor(&self) -> Element<'_, Message> {
        let l = &self.cfg.layers[self.layer].lighting;
        let rgb = config::parse_color(&l.color);
        let [r, g, b] = rgb.unwrap_or([255, 255, 255]);
        let effect = tooltip(
            pick_list(&EFFECT_CHOICES[..], Some(EffectChoice(l.effect)), |e| {
                Message::Lighting(LightingEdit::Effect(e.0))
            })
            .width(220)
            .menu_height(320)
            .style(style::field),
            tip("The action 'RGB effect next' cycles through these effects, in this order."),
            tooltip::Position::Top,
        );
        let channel = |i: usize, name: &'static str, v: u8| {
            row![
                text(name).size(12).width(14).style(style::muted),
                slider(0..=255, v, move |v| Message::Lighting(LightingEdit::Channel(i, v))),
                text(v).size(12).width(28).align_x(Alignment::End).style(style::muted),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
        };
        let colour = row![
            container(space())
                .width(32)
                .height(32)
                .style(style::swatch(rgb.map_or(Color::TRANSPARENT, |[r, g, b]| Color::from_rgb8(r, g, b)))),
            text_input("#RRGGBB", &l.color)
                .on_input(|s| Message::Lighting(LightingEdit::Color(s)))
                .font(Font::MONOSPACE)
                .width(110)
                .style(style::input),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        let channels = row![
            space().width(LABEL_WIDTH),
            column![channel(0, "R", r), channel(1, "G", g), channel(2, "B", b)].spacing(6),
        ]
        .spacing(12);
        let shared = checkbox(l.shared)
            .label("Same lighting on every layer")
            .on_toggle(|on| Message::Lighting(LightingEdit::Shared(on)));
        let key_color = |k: usize| {
            let value = l.key_colors.as_ref().and_then(|c| c.get(k)).map_or("", String::as_str);
            column![
                text(format!("K{}", k + 1)).size(11).style(style::muted),
                text_input(NO_COLOR, if value == NO_COLOR { "" } else { value })
                    .on_input(move |s| Message::Lighting(LightingEdit::KeyColor(k, s)))
                    .font(Font::MONOSPACE)
                    .size(13)
                    .width(80)
                    .style(style::input),
            ]
            .spacing(2)
        };
        let key_colors = column![
            row((0..config::KEYS / 2).map(|k| key_color(k).into())).spacing(8),
            row((config::KEYS / 2..config::KEYS).map(|k| key_color(k).into())).spacing(8),
        ]
        .spacing(8);
        let body = column![
            field_row("Effect", effect),
            value_row(
                "Brightness",
                slider(0..=config::MAX_BRIGHTNESS, l.brightness, |v| Message::Lighting(LightingEdit::Brightness(v))),
                l.brightness.to_string()
            ),
            value_row("Speed", slider(0..=255, l.speed, |v| Message::Lighting(LightingEdit::Speed(v))), l.speed.to_string()),
            field_row("Colour", colour),
            channels,
            field_row("Key colours", key_colors),
            row![space().width(LABEL_WIDTH), shared].spacing(12),
        ]
        .push(l.extra.is_some().then(|| {
            hint("This layer has per-key data bnk8 can't show; editing a key colour replaces it.")
        }))
        .spacing(12);
        card("Lighting".into(), body)
    }

    fn macros_tab(&self) -> Element<'_, Message> {
        let list = (0..actions::MACRO_SLOTS).map(|m| {
            let n = self.cfg.macros.get(&config::macro_key(m)).map_or(0, |x| key_steps(&x.steps));
            let count = match n {
                0 => "empty".to_string(),
                1 => "1 step".to_string(),
                n => format!("{n} steps"),
            };
            let title = column![text(format!("Macro {}", m + 1)).size(14)]
                .push(self.macro_name(m).map(|name| text(elide(name, 22)).size(12).style(style::muted)))
                .width(Fill);
            let content = row![title, text(count).size(12).style(if n == 0 { style::section } else { style::muted })]
                .align_y(Alignment::Center);
            nav_button(content, self.macro_slot == m, Message::SelectMacro(m))
        });
        column![
            page_title(
                "Macros".into(),
                "Up to 15 macros of 7 key steps each. Bind one by picking Macro and its number in any key or knob slot."
            ),
            self.ai_card(),
            row![
                container(Column::with_children(list).spacing(2)).padding(8).width(230).style(style::card),
                self.macro_editor(),
            ]
            .spacing(20),
        ]
        .spacing(22)
        .into()
    }

    fn macro_editor(&self) -> Element<'_, Message> {
        let m = self.macro_slot;
        let mac = self.cfg.macros.get(&config::macro_key(m));
        let steps: &[String] = mac.map_or(&[], |x| x.steps.as_slice());
        let n = steps.len();
        let rows = steps.iter().enumerate().map(|(i, step)| {
            let (verb, arg) = parse_step(step);
            let verb_pick = pick_list(Verb::ALL, verb, move |v| Message::Step(i, StepEdit::Verb(v)))
                .placeholder("Raw")
                .width(110)
                .style(style::field);
            let editor: Element<'_, Message> = match verb {
                Some(Verb::Delay) => {
                    let ms: u16 = arg.parse().unwrap_or(0);
                    row![
                        slider(0..=2550, ms, move |v| Message::Step(i, StepEdit::Delay(v))).step(10u16).width(Fill),
                        text(format!("{ms} ms")).size(12).width(64).align_x(Alignment::End).style(style::muted),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center)
                    .width(Length::Fixed(200.0))
                    .into()
                }
                Some(_) => key_picker(&self.catalog, arg, move |k| Message::Step(i, StepEdit::Key(k))),
                None => text_input("raw.…", step)
                    .on_input(move |s| Message::Step(i, StepEdit::Raw(s)))
                    .font(Font::MONOSPACE)
                    .width(200)
                    .style(style::input)
                    .into(),
            };
            let icon = |label: &'static str, enabled: bool, edit: StepEdit| {
                button(text(label).size(13))
                    .style(style::icon)
                    .padding([4, 8])
                    .on_press_maybe(enabled.then_some(Message::Step(i, edit)))
            };
            row![
                text(format!("{}", i + 1)).size(12).width(22).style(style::muted),
                verb_pick,
                editor,
                space::horizontal(),
                icon("↑", i > 0, StepEdit::Up),
                icon("↓", i + 1 < n, StepEdit::Down),
                icon("✕", true, StepEdit::Remove),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into()
        });
        let list: Element<'_, Message> = if n == 0 {
            text("No steps yet.").size(13).style(style::muted).into()
        } else {
            Column::with_children(rows).spacing(8).into()
        };
        let actions = row![
            button(text("+ Key step").size(13))
                .style(style::ghost)
                .on_press_maybe((key_steps(steps) < config::MACRO_STEPS).then_some(Message::AddKeyStep)),
            button(text("+ Delay").size(13)).style(style::ghost).on_press(Message::AddDelay),
            space::horizontal(),
            button(text("Clear").size(13)).style(style::ghost_danger).on_press_maybe((n > 0).then_some(Message::ClearSteps)),
        ]
        .spacing(8);
        let placeholder = if mac.is_some() { config::default_macro_name(m) } else { "Add a step to name it".into() };
        let name = text_input(&placeholder, self.macro_name(m).unwrap_or_default())
            .on_input_maybe(mac.is_some().then_some(Message::MacroName))
            .width(280)
            .style(style::input);
        let unsaved = self
            .connected
            .as_ref()
            .is_some_and(|c| !c.store)
            .then(|| hint("This keypad keeps names only once Binepad Studio has saved to it; Export TOML keeps them."));
        card(
            format!("Macro {}", m + 1),
            column![field_row("Name", name)]
                .push(unsaved)
                .push(list)
                .push(actions)
                .push(hint("A delay (multiple of 10 ms, up to 2550 ms) waits before the next key step."))
                .spacing(14),
        )
    }

    /// `macro.N` -> the macro's name, for macros with one.
    fn macro_labels(&self) -> BTreeMap<String, String> {
        (0..actions::MACRO_SLOTS)
            .filter_map(|m| Some((format!("macro.{}", config::macro_key(m)), self.macro_name(m)?.to_string())))
            .collect()
    }

    /// "Ask Claude": sign-in state, the prompt, and Claude's proposed macros.
    fn ai_card(&self) -> Element<'_, Message> {
        let check = |enabled: bool| {
            button(text("Check again").size(13))
                .style(style::ghost)
                .on_press_maybe(enabled.then_some(Message::Ai(AiEvent::Check)))
        };
        let body: Element<'_, Message> = match &self.ai.login {
            Login::Unchecked | Login::Checking => hint("Looking for Claude Code…"),
            Login::Missing => column![
                text("Ask Claude needs Claude Code, signed in with your Claude account.").size(13),
                hint("Install it from claude.com/claude-code, then press Check again."),
                check(true),
            ]
            .spacing(10)
            .into(),
            Login::SignedOut(_) => column![
                text("Sign in with your Claude account, then describe a macro in your own words.").size(13),
                button(text("Sign in with Claude").size(13).font(BOLD))
                    .style(style::primary)
                    .padding([6, 16])
                    .on_press(Message::Ai(AiEvent::SignIn)),
                hint("Opens your browser. Sign-in goes through Claude Code; bnk8 never sees your password."),
            ]
            .spacing(10)
            .into(),
            Login::SigningIn => column![
                text("Finish signing in in your browser…").size(13),
                hint("No browser window? Run `claude auth login` in Terminal, then press Check again."),
                check(true),
            ]
            .spacing(10)
            .into(),
            Login::Failed(e) => column![error_box(e), check(true)].spacing(10).into(),
            Login::SignedIn { account, .. } => {
                let asking = self.ai.asking;
                let can_ask = !asking && !self.ai.prompt.trim().is_empty();
                let status = row![
                    status_pill(Tone::Success, format!("Signed in as {account}")),
                    space::horizontal(),
                    check(!asking),
                ]
                .align_y(Alignment::Center);
                let ask = row![
                    text_input("Describe a macro, e.g. Cmd+Shift+4 to capture part of the screen", &self.ai.prompt)
                        .on_input(|s| Message::Ai(AiEvent::Prompt(s)))
                        .on_submit(Message::Ai(AiEvent::Ask))
                        .width(Fill)
                        .style(style::input),
                    button(text(if asking { "Asking…" } else { "Ask Claude" }).size(13).font(BOLD))
                        .style(style::primary)
                        .padding([6, 16])
                        .on_press_maybe(can_ask.then_some(Message::Ai(AiEvent::Ask))),
                ]
                .spacing(10)
                .align_y(Alignment::Center);
                let answer: Option<Element<'_, Message>> = match &self.ai.answer {
                    None => None,
                    Some(Err(e)) => Some(error_box(e)),
                    Some(Ok(a)) => Some(self.ai_answer(a)),
                };
                column![status, ask]
                    .push(answer)
                    .push(hint(
                        "Claude's macros are added as edits; nothing is written until Apply to device. Uses your Claude subscription through Claude Code.",
                    ))
                    .spacing(12)
                    .into()
            }
        };
        card("Ask Claude".into(), body)
    }

    /// Claude's reply and proposed macros, with Add to macros / Dismiss.
    fn ai_answer<'a>(&'a self, a: &'a Answer) -> Element<'a, Message> {
        let proposals = a.macros.iter().map(|p| {
            let default = config::default_macro_name(p.slot);
            let note = self.cfg.macros.get(&config::macro_key(p.slot)).and_then(|old| {
                if old.steps != p.steps {
                    Some(format!("Replaces its current {} step(s).", key_steps(&old.steps)))
                } else if self.macro_name(p.slot).unwrap_or(&default) != p.name {
                    Some("Renames it; the steps stay the same.".into())
                } else {
                    None
                }
            });
            let details = column![
                text(&p.name).size(13),
                text(p.steps.join(" · ")).size(12).font(Font::MONOSPACE).style(style::muted),
            ]
            .push(note.map(|s| text(s).size(12).style(style::muted)))
            .spacing(4);
            row![text(format!("Macro {}", p.slot + 1)).font(SEMIBOLD).width(80), details].spacing(12).into()
        });
        let add = (!a.macros.is_empty()).then(|| {
            button(text("Add to macros").size(13).font(BOLD))
                .style(style::primary)
                .padding([6, 16])
                .on_press(Message::Ai(AiEvent::Accept))
        });
        let actions = row![]
            .push(add)
            .push(button(text("Dismiss").size(13)).style(style::ghost).on_press(Message::Ai(AiEvent::Dismiss)))
            .spacing(8)
            .align_y(Alignment::Center);
        column![text(&a.reply).size(13)]
            .push((!a.macros.is_empty()).then(|| Column::with_children(proposals).spacing(10)))
            .push(actions)
            .spacing(12)
            .into()
    }

    fn timing_tab(&self) -> Element<'_, Message> {
        let t = &self.cfg.timing;
        column![
            page_title("Timing".into(), "Applies to every key and the knob press."),
            card(
                "Gestures".into(),
                column![
                    value_row(
                        "Double-tap window",
                        slider(100..=800, t.double_tap_ms, Message::DoubleTap),
                        format!("{} ms", t.double_tap_ms)
                    ),
                    value_row("Hold threshold", slider(150..=2000, t.hold_ms, Message::Hold), format!("{} ms", t.hold_ms)),
                ]
                .spacing(14),
            ),
        ]
        .spacing(22)
        .max_width(640)
        .into()
    }

    fn footer(&self) -> Element<'_, Message> {
        let idle = self.busy.is_none();
        let connected = self.connected.is_some();
        let target = self.target().is_some();
        let lines = match &self.preview {
            Preview::Ready { changes: Changes::Items(lines), .. } => lines.as_slice(),
            _ => &[],
        };
        let (tone, summary) = match &self.preview {
            Preview::Invalid(e) => (Tone::Danger, format!("Invalid: {e}")),
            Preview::Ready { changes: Changes::Unknown, .. } => {
                (Tone::Accent, "Device config unknown: Apply writes everything".into())
            }
            Preview::Ready { .. } if lines.is_empty() => (Tone::Neutral, "No changes".into()),
            Preview::Ready { .. } => (Tone::Accent, format!("{} pending change(s)", lines.len())),
        };
        let (tone, summary) = match (connected, target) {
            (false, _) => (tone, format!("{summary} (not connected)")),
            (true, false) => (Tone::Danger, "Selected device not loaded: press Load from device".into()),
            (true, true) => (tone, summary),
        };
        let can_apply = idle && target && self.pending().is_some_and(|n| n > 0);
        let toggle = (!lines.is_empty()).then(|| {
            button(text(if self.show_changes { "Hide changes" } else { "Show changes" }).size(12))
                .style(style::icon)
                .padding([4, 8])
                .on_press(Message::ToggleChanges)
        });
        let bar = row![
            container(space()).width(8).height(8).style(style::dot(tone)),
            text(summary).size(13).font(SEMIBOLD),
        ]
        .push(toggle)
        .push(space::horizontal())
        .push(button(text("Import TOML…").size(13)).style(style::ghost).on_press(Message::Import))
        .push(button(text("Export TOML…").size(13)).style(style::ghost).on_press(Message::Export))
        .push(
            button(text("Factory reset…").size(13))
                .style(style::ghost_danger)
                .on_press_maybe((idle && target).then_some(Message::AskReset)),
        )
        .push(rule::vertical(1).style(rule::weak))
        .push(button(text("Discard edits").size(13)).style(style::ghost).on_press_maybe(idle.then_some(Message::Discard)))
        .push(
            button(text("Apply to device").size(13).font(BOLD))
                .style(style::primary)
                .padding([6, 16])
                .on_press_maybe(can_apply.then_some(Message::Apply)),
        )
        .spacing(10)
        .height(34)
        .align_y(Alignment::Center);
        let changes = (self.show_changes && !lines.is_empty()).then(|| {
            container(scrollable(
                Column::with_children(lines.iter().map(|l| text(l).size(12).font(Font::MONOSPACE).into())).spacing(2),
            ))
            .max_height(140)
            .width(Fill)
            .padding(10)
            .style(style::card)
        });
        column![].push(changes).push(bar).spacing(10).padding([10, 20]).into()
    }

    fn reset_dialog(&self) -> Element<'_, Message> {
        let dialog = container(
            column![
                text("Restore factory config?").size(20).font(BOLD),
                text("Replaces every layer, binding, macro and lighting setting on the device.").size(14),
                row![
                    space::horizontal(),
                    button("Cancel").style(style::ghost).on_press(Message::CancelReset),
                    button("Restore factory config").style(style::danger).on_press(Message::ConfirmReset),
                ]
                .spacing(10),
            ]
            .spacing(16),
        )
        .width(440)
        .padding(24)
        .style(style::dialog);
        opaque(mouse_area(center(opaque(dialog)).style(style::backdrop)).on_press(Message::CancelReset))
    }
}

// ---- view helpers ------------------------------------------------------

/// A key's binding in `slot`, borrowed; the `none` placeholder reads as unbound (as in `Key::slots`).
fn key_slot<'a>(key: &'a Key, slot: &str) -> Option<&'a str> {
    match key {
        Key::Tap(a) if slot == "tap" && a != "none" => Some(a),
        Key::Tap(_) => None,
        Key::Slots(s) => s.get(slot).map(String::as_str),
    }
}

fn page_title<'a>(title: String, subtitle: &'a str) -> Element<'a, Message> {
    column![text(title).size(26).font(BOLD), text(subtitle).size(13).style(style::muted)].spacing(4).into()
}

fn section_label(label: &str) -> Element<'_, Message> {
    text(label).size(11).font(BOLD).style(style::section).into()
}

fn hint(body: &str) -> Element<'_, Message> {
    text(body).size(12).style(style::muted).into()
}

fn error_box(e: &str) -> Element<'_, Message> {
    container(text(e).size(13)).padding([8, 12]).width(Fill).style(style::banner(Tone::Danger)).into()
}

fn card<'a>(title: String, body: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(column![text(title).size(15).font(BOLD), body.into()].spacing(14))
        .padding(18)
        .width(Fill)
        .style(style::card)
        .into()
}

fn field_row<'a>(label: &'a str, field: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    row![text(label).size(13).width(LABEL_WIDTH), field.into()].spacing(12).align_y(Alignment::Center).into()
}

fn value_row<'a>(label: &'a str, field: impl Into<Element<'a, Message>>, value: String) -> Element<'a, Message> {
    row![
        text(label).size(13).width(LABEL_WIDTH),
        container(field).width(Fill),
        text(value).size(12).font(Font::MONOSPACE).width(56).align_x(Alignment::End).style(style::muted),
    ]
    .spacing(12)
    .align_y(Alignment::Center)
    .into()
}

fn nav_button<'a>(content: impl Into<Element<'a, Message>>, selected: bool, on_press: Message) -> Element<'a, Message> {
    button(content).width(Fill).padding([7, 10]).style(style::nav(selected)).on_press(on_press).into()
}

fn status_pill<'a>(tone: Tone, label: String) -> Element<'a, Message> {
    container(
        row![container(space()).width(7).height(7).style(style::dot(tone)), text(label).size(12)]
            .spacing(7)
            .align_y(Alignment::Center),
    )
    .padding([5, 12])
    .style(style::pill(tone))
    .into()
}

/// Shortens `s` to at most `max` characters, ending in an ellipsis.
fn elide(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

fn key_steps(steps: &[String]) -> usize {
    steps.iter().filter(|s| !s.starts_with("delay")).count()
}

/// Points `layer.N`, `layer.mo.N` and `layer.tg.N` bindings at the right
/// layer after layer `removed` was deleted: later layers shift down by one,
/// bindings to the removed layer are unbound. Returns how many bindings were
/// unbound.
fn renumber_layer_refs(cfg: &mut Config, removed: usize) -> usize {
    let mut cleared = 0;
    for layer in &mut cfg.layers {
        for key in &mut layer.keys {
            let mut slots = key.slots();
            let (changed, n) = renumber_slots(&mut slots, &KEY_SLOTS, removed);
            if changed {
                *key = Key::from_slots(slots);
            }
            cleared += n;
        }
        cleared += renumber_slots(&mut layer.knob, &KNOB_SLOTS, removed).1;
    }
    cleared
}

/// `renumber_layer_refs` for one key or knob: whether any slot changed, and
/// how many were unbound.
fn renumber_slots(slots: &mut Slots, names: &[&str], removed: usize) -> (bool, usize) {
    let (mut changed, mut cleared) = (false, 0);
    for name in names {
        let slot = slots.slot_mut(name).expect("slot name");
        let Some(n) = slot.as_deref().and_then(actions::layer_target) else { continue };
        if n < removed {
            continue;
        }
        if n == removed {
            *slot = None;
            cleared += 1;
        } else {
            *slot = slot.as_deref().and_then(|s| actions::with_layer_target(s, n - 1));
        }
        changed = true;
    }
    (changed, cleared)
}

fn layer_color(layer: &Layer) -> Color {
    let [r, g, b] = config::parse_color(&layer.lighting.color).unwrap_or([0, 0, 0]);
    if layer.lighting.effect == 0 { Color::from_rgb8(40, 42, 50) } else { Color::from_rgb8(r, g, b) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removing_a_layer_renumbers_layer_bindings() {
        let mut base = config::from_blob(FACTORY).unwrap().layers.remove(0);
        base.keys = vec![Key::Tap("none".into()); config::KEYS];
        base.knob = Slots::default();
        let mut cfg = config::from_blob(FACTORY).unwrap();
        cfg.layers = vec![base.clone(), base.clone(), base];
        let keys = &mut cfg.layers[0].keys;
        keys[0] = Key::Tap("layer.3".into());
        keys[1] = Key::Slots(Slots { tap: Some("layer.2".into()), hold: Some("layer.3".into()), ..Slots::default() });
        keys[2] = Key::Tap("layer.1".into());
        keys[3] = Key::Tap("keyboard.a".into());
        keys[4] = Key::Tap("layer.next".into());
        keys[5] = Key::Tap("layer.2".into());
        keys[6] = Key::Tap("layer.02".into());
        keys[7] = Key::Tap("layer.mo.3".into());
        cfg.layers[2].knob = Slots {
            hold: Some("layer.next".into()),
            ccw: Some("layer.2".into()),
            cw: Some("layer.3".into()),
            ..Slots::default()
        };

        cfg.layers.remove(1);
        assert_eq!(renumber_layer_refs(&mut cfg, 1), 3);

        let keys = &cfg.layers[0].keys;
        assert_eq!(keys[0], Key::Tap("layer.2".into()));
        assert_eq!(keys[1], Key::Slots(Slots { hold: Some("layer.2".into()), ..Slots::default() }));
        assert_eq!(keys[2], Key::Tap("layer.1".into()));
        assert_eq!(keys[3], Key::Tap("keyboard.a".into()));
        assert_eq!(keys[4], Key::Tap("layer.next".into()));
        assert_eq!(keys[5], Key::Tap("none".into()));
        assert_eq!(keys[6], Key::Tap("layer.02".into()));
        assert_eq!(keys[7], Key::Tap("layer.mo.2".into()));
        assert_eq!(
            cfg.layers[1].knob,
            Slots { hold: Some("layer.next".into()), cw: Some("layer.2".into()), ..Slots::default() }
        );
    }
}
