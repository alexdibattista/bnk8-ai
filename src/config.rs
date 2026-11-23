//! TOML config model and its mapping onto the 2000-byte config blob.
//!
//! Layout per PROTOCOL.md "Config blob". `to_blob` starts from a base blob
//! (the device's current config) and overwrites every modelled region, so
//! bytes this model does not cover survive an apply unchanged.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::actions::{self, Action, ENTRY_LEN, MACRO_SLOTS, MAX_LAYERS};
use crate::protocol::{CONFIG_LEN, crc32};

pub type Blob = [u8; CONFIG_LEN];

/// Device identifier string embedded in the firmware.
pub const DEVICE: &str = "binepad.bnk8";
/// Factory default config, extracted from the published 6.0.27 image.
pub static FACTORY: &Blob = include_bytes!("../assets/factory-config.bin");

const MAGIC: [u8; 4] = *b"BPN8";
const OFF_LAYERS: usize = 5;
const OFF_POSITIONS: usize = 6;
const OFF_KNOB_SLOTS: usize = 7;
const OFF_ACTION_COUNT: usize = 8;
const OFF_DOUBLE_TAP: usize = 0x0e;
const OFF_HOLD: usize = 0x10;
const OFF_INNER_CRC: usize = 0x14;
const OFF_ACTIONS: usize = 0x18;
const MAX_ACTIONS: usize = 96;
const OFF_KEYMAP: usize = 0x2b8;
const KEYMAP_LEN: usize = 0x26;
const OFF_MACROS: usize = 0x434;
const MACRO_LEN: usize = 0x1d;
const MACRO_STEPS: usize = 7;
const OFF_LIGHTING: usize = 0x5e7;
const LIGHTING_LEN: usize = 0x28;
const LIGHTING_EXTRA: usize = 7;

/// Keymap offsets of K1..K8 (matrix positions 0, 1, 3..=8; position 2 is the knob).
const KEY_OFFSETS: [usize; 8] = [0, 4, 14, 18, 22, 26, 30, 34];
const KNOB_OFFSET: usize = 8;
/// Key slots: tap, double tap, hold, tap-then-hold. The knob adds ccw, cw.
pub const KEY_SLOTS: [&str; 4] = ["tap", "double_tap", "hold", "tap_hold"];
pub const KNOB_SLOTS: [&str; 6] = ["tap", "double_tap", "hold", "tap_hold", "ccw", "cw"];

pub const MAX_EFFECT: u8 = 50;
pub const MAX_BRIGHTNESS: u8 = 180;
const NONE: &str = "none";

/// TOML format bnk8 writes. Format 1 (no `format` key) used bnk8's own
/// action names and 0-based `layer.N`/`macro.N`; it is migrated on read.
pub const FORMAT: u32 = 2;

fn legacy_format() -> u32 {
    1
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// First, so it is written first.
    #[serde(default = "legacy_format")]
    pub format: u32,
    pub device: String,
    pub timing: Timing,
    pub layers: Vec<Layer>,
    /// Macro slot (`"1"`..`"15"`, see [`macro_key`]) -> steps; referenced as `macro.N`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub macros: BTreeMap<String, Vec<String>>,
}

/// The `[macros]` key (and `macro.N` number) of 0-based macro slot `slot`.
pub fn macro_key(slot: usize) -> String {
    (slot + 1).to_string()
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Timing {
    pub double_tap_ms: u16,
    pub hold_ms: u16,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    /// K1..K8: an action name (tap only) or a table of slots.
    pub keys: Vec<Key>,
    pub knob: Slots,
    pub lighting: Lighting,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum Key {
    Tap(String),
    Slots(Slots),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Slots {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tap: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub double_tap: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tap_hold: Option<String>,
    /// Knob only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ccw: Option<String>,
    /// Knob only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cw: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Lighting {
    /// Effect id 0..=50; 0 = off.
    pub effect: u8,
    /// 0..=180.
    pub brightness: u8,
    pub speed: u8,
    /// `#RRGGBB`.
    pub color: String,
    /// Raw flags byte (factory: 1).
    pub flags: u8,
    /// Undecoded trailing 33 bytes as hex; omitted when all zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<String>,
}

impl Slots {
    pub fn get(&self, slot: &str) -> Option<&String> {
        self.slot_ref(slot).and_then(|s| s.as_ref())
    }

    /// Mutable access by slot name; `None` for an unknown slot name.
    pub fn slot_mut(&mut self, slot: &str) -> Option<&mut Option<String>> {
        Some(match slot {
            "tap" => &mut self.tap,
            "double_tap" => &mut self.double_tap,
            "hold" => &mut self.hold,
            "tap_hold" => &mut self.tap_hold,
            "ccw" => &mut self.ccw,
            "cw" => &mut self.cw,
            _ => return None,
        })
    }

    fn slot_ref(&self, slot: &str) -> Option<&Option<String>> {
        Some(match slot {
            "tap" => &self.tap,
            "double_tap" => &self.double_tap,
            "hold" => &self.hold,
            "tap_hold" => &self.tap_hold,
            "ccw" => &self.ccw,
            "cw" => &self.cw,
            _ => return None,
        })
    }
}

impl Key {
    /// The key's slots; the `none` placeholder reads as unbound.
    pub fn slots(&self) -> Slots {
        match self {
            Key::Tap(a) if a == NONE => Slots::default(),
            Key::Tap(a) => Slots { tap: Some(a.clone()), ..Slots::default() },
            Key::Slots(s) => s.clone(),
        }
    }

    /// Canonical form: a bare string when only `tap` is bound.
    pub fn from_slots(s: Slots) -> Key {
        let only_tap = s.double_tap.is_none() && s.hold.is_none() && s.tap_hold.is_none();
        match (&s.tap, only_tap) {
            (None, true) => Key::Tap(NONE.into()),
            (Some(a), true) => Key::Tap(a.clone()),
            _ => Key::Slots(s),
        }
    }
}

fn u16_at(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Decodes a blob. Fails only on blobs the firmware itself would reject.
pub fn from_blob(b: &Blob) -> Result<Config> {
    ensure!(b[..4] == MAGIC, "config magic {:02x?}, expected BPN8", &b[..4]);
    let layers = b[OFF_LAYERS] as usize;
    ensure!((1..=MAX_LAYERS).contains(&layers), "config has {layers} layers");
    let count = b[OFF_ACTION_COUNT] as usize;
    ensure!(count <= MAX_ACTIONS, "config has {count} actions");
    let table: Vec<Action> = (0..count)
        .map(|i| {
            let o = OFF_ACTIONS + i * ENTRY_LEN;
            Action::from_bytes(b[o..o + ENTRY_LEN].try_into().unwrap())
        })
        .collect();
    let action = |idx: u8| -> Result<Option<String>> {
        match idx as usize {
            0 => Ok(None),
            i if i <= count => Ok(Some(actions::decode(table[i - 1]))),
            i => bail!("keymap references action {i} of {count}"),
        }
    };
    let slots = |km: &[u8], names: &[&str]| -> Result<Slots> {
        let mut s = Slots::default();
        for (i, name) in names.iter().enumerate() {
            *s.slot_mut(name).unwrap() = action(km[i])?;
        }
        Ok(s)
    };

    let mut out = Vec::with_capacity(layers);
    for p in 0..layers {
        let km = &b[OFF_KEYMAP + p * KEYMAP_LEN..][..KEYMAP_LEN];
        let keys = KEY_OFFSETS
            .iter()
            .map(|&o| slots(&km[o..], &KEY_SLOTS).map(Key::from_slots))
            .collect::<Result<_>>()?;
        let knob = slots(&km[KNOB_OFFSET..], &KNOB_SLOTS)?;
        let l = &b[OFF_LIGHTING + p * LIGHTING_LEN..][..LIGHTING_LEN];
        let extra = &l[LIGHTING_EXTRA..];
        out.push(Layer {
            keys,
            knob,
            lighting: Lighting {
                effect: l[0],
                brightness: l[1],
                speed: l[2],
                flags: l[3],
                color: format!("#{:02X}{:02X}{:02X}", l[4], l[5], l[6]),
                extra: extra.iter().any(|&x| x != 0).then(|| hex(extra)),
            },
        });
    }

    let mut macros = BTreeMap::new();
    for m in 0..MACRO_SLOTS {
        let raw = &b[OFF_MACROS + m * MACRO_LEN..][..MACRO_LEN];
        let n = raw[0] as usize;
        if n == 0 {
            continue;
        }
        ensure!(n <= MACRO_STEPS, "macro {m} has {n} steps");
        let mut steps = Vec::new();
        for s in raw[1..].chunks(4).take(n) {
            steps.extend(decode_step(s.try_into().unwrap()));
        }
        macros.insert(macro_key(m), steps);
    }

    Ok(Config {
        format: FORMAT,
        device: DEVICE.into(),
        timing: Timing {
            double_tap_ms: u16_at(b, OFF_DOUBLE_TAP),
            hold_ms: u16_at(b, OFF_HOLD),
        },
        layers: out,
        macros,
    })
}

/// Macro step on the wire: `[mode][code u16][delay ×10 ms before the step]`.
/// Mode 1 = press, 2 = release, anything else = tap (we write 0).
fn decode_step(s: [u8; 4]) -> Vec<String> {
    let code = u16::from_le_bytes([s[1], s[2]]);
    let verb = match s[0] {
        0 => "tap",
        1 => "press",
        2 => "release",
        _ => return vec![format!("raw.{}", hex(&s))],
    };
    let Some(key) = actions::key_name(code) else {
        return vec![format!("raw.{}", hex(&s))];
    };
    let mut out = Vec::new();
    if s[3] != 0 {
        out.push(format!("delay {}", s[3] as u32 * 10));
    }
    out.push(format!("{verb} {key}"));
    out
}

fn encode_steps(steps: &[String]) -> Result<Vec<[u8; 4]>> {
    let mut out = Vec::new();
    let mut delay: u32 = 0;
    for (i, step) in steps.iter().enumerate() {
        let ctx = || format!("[{i}] `{step}`");
        if let Some(raw) = step.strip_prefix("raw.") {
            let bytes = unhex(raw).filter(|v| v.len() == 4).with_context(|| {
                format!("{}: raw steps are `raw.` + 8 hex digits", ctx())
            })?;
            ensure!(delay == 0, "{}: a delay cannot precede a raw step", ctx());
            out.push(bytes.try_into().unwrap());
            continue;
        }
        let (verb, arg) = step
            .split_once(' ')
            .with_context(|| format!("{}: expected `tap|press|release <key>` or `delay <ms>`", ctx()))?;
        let mode = match verb {
            "delay" => {
                let ms: u32 = arg.parse().with_context(|| format!("{}: bad delay", ctx()))?;
                ensure!(ms.is_multiple_of(10), "{}: delay must be a multiple of 10 ms", ctx());
                delay += ms / 10;
                ensure!(delay <= 255, "{}: delay before one step exceeds 2550 ms", ctx());
                continue;
            }
            "tap" => 0,
            "press" => 1,
            "release" => 2,
            _ => bail!("{}: unknown verb `{verb}`", ctx()),
        };
        let code = actions::encode_key(arg).with_context(ctx)?;
        let c = code.to_le_bytes();
        out.push([mode, c[0], c[1], delay as u8]);
        delay = 0;
    }
    ensure!(delay == 0, "trailing delay has no step to delay");
    ensure!(out.len() <= MACRO_STEPS, "{} steps, the device stores at most {MACRO_STEPS}", out.len());
    Ok(out)
}

fn parse_color(s: &str) -> Option<[u8; 3]> {
    let h = s.strip_prefix('#').filter(|h| h.len() == 6)?;
    unhex(h)?.try_into().ok()
}

/// Encodes `cfg` over `base`, rewriting every modelled region.
pub fn to_blob(cfg: &Config, base: &Blob) -> Result<Blob> {
    ensure!(cfg.format == FORMAT, "format: {} is not {FORMAT}", cfg.format);
    ensure!(cfg.device == DEVICE, "device: `{}` does not match `{DEVICE}`", cfg.device);
    let n_layers = cfg.layers.len();
    ensure!((1..=MAX_LAYERS).contains(&n_layers), "layers: {n_layers} layers, expected 1..={MAX_LAYERS}");
    let t = &cfg.timing;
    ensure!((100..=800).contains(&t.double_tap_ms), "timing.double_tap_ms: {} not in 100..=800", t.double_tap_ms);
    ensure!((150..=2000).contains(&t.hold_ms), "timing.hold_ms: {} not in 150..=2000", t.hold_ms);

    // Resolve every binding to an action first, so the table can be built.
    let mut keymaps = Vec::with_capacity(n_layers);
    for (p, layer) in cfg.layers.iter().enumerate() {
        ensure!(layer.keys.len() == 8, "layers[{p}].keys: {} keys, expected 8", layer.keys.len());
        let mut km: Vec<(usize, Option<Action>)> = Vec::new();
        for (k, key) in layer.keys.iter().enumerate() {
            let s = key.slots();
            for slot in ["ccw", "cw"] {
                ensure!(s.get(slot).is_none(), "layers[{p}].keys[{k}].{slot}: only the knob turns");
            }
            for (i, slot) in KEY_SLOTS.iter().enumerate() {
                let a = resolve(s.get(slot), cfg).with_context(|| format!("layers[{p}].keys[{k}].{slot}"))?;
                km.push((KEY_OFFSETS[k] + i, a));
            }
        }
        for (i, slot) in KNOB_SLOTS.iter().enumerate() {
            let a = resolve(layer.knob.get(slot), cfg).with_context(|| format!("layers[{p}].knob.{slot}"))?;
            km.push((KNOB_OFFSET + i, a));
        }
        keymaps.push(km);
    }

    // Action table: keep surviving base entries in base order, append new ones.
    let base_count = (base[OFF_ACTION_COUNT] as usize).min(MAX_ACTIONS);
    let used: Vec<Action> = keymaps.iter().flatten().filter_map(|(_, a)| *a).collect();
    let mut table: Vec<Action> = Vec::new();
    for i in 0..base_count {
        let o = OFF_ACTIONS + i * ENTRY_LEN;
        let a = Action::from_bytes(base[o..o + ENTRY_LEN].try_into().unwrap());
        if used.contains(&a) && !table.contains(&a) {
            table.push(a);
        }
    }
    for a in &used {
        if !table.contains(a) {
            table.push(*a);
        }
    }
    ensure!(table.len() <= MAX_ACTIONS, "{} distinct actions, the device stores at most {MAX_ACTIONS}", table.len());

    let mut b = *base;
    b[..4].copy_from_slice(&MAGIC);
    b[4] = 1;
    b[OFF_LAYERS] = n_layers as u8;
    b[OFF_POSITIONS] = 9;
    b[OFF_KNOB_SLOTS] = 6;
    b[OFF_ACTION_COUNT] = table.len() as u8;
    b[OFF_DOUBLE_TAP..OFF_DOUBLE_TAP + 2].copy_from_slice(&t.double_tap_ms.to_le_bytes());
    b[OFF_HOLD..OFF_HOLD + 2].copy_from_slice(&t.hold_ms.to_le_bytes());

    b[OFF_ACTIONS..OFF_KEYMAP].fill(0);
    for (i, a) in table.iter().enumerate() {
        b[OFF_ACTIONS + i * ENTRY_LEN..][..ENTRY_LEN].copy_from_slice(&a.to_bytes());
    }

    // Keymaps and lighting for all 10 layer slots: the firmware validates
    // every slot, so unused ones are zeroed.
    b[OFF_KEYMAP..OFF_MACROS].fill(0);
    for (p, km) in keymaps.iter().enumerate() {
        for (off, a) in km {
            if let Some(a) = a {
                let idx = table.iter().position(|t| t == a).unwrap() + 1;
                b[OFF_KEYMAP + p * KEYMAP_LEN + off] = idx as u8;
            }
        }
    }
    b[OFF_LIGHTING..OFF_LIGHTING + MAX_LAYERS * LIGHTING_LEN].fill(0);
    for (p, layer) in cfg.layers.iter().enumerate() {
        let l = &layer.lighting;
        let path = format!("layers[{p}].lighting");
        ensure!(l.effect <= MAX_EFFECT, "{path}.effect: {} not in 0..={MAX_EFFECT}", l.effect);
        ensure!(l.brightness <= MAX_BRIGHTNESS, "{path}.brightness: {} not in 0..={MAX_BRIGHTNESS}", l.brightness);
        let rgb = parse_color(&l.color).with_context(|| format!("{path}.color: `{}` is not #RRGGBB", l.color))?;
        let out = &mut b[OFF_LIGHTING + p * LIGHTING_LEN..][..LIGHTING_LEN];
        out[..7].copy_from_slice(&[l.effect, l.brightness, l.speed, l.flags, rgb[0], rgb[1], rgb[2]]);
        if let Some(x) = &l.extra {
            let bytes = unhex(x)
                .filter(|v| v.len() == LIGHTING_LEN - LIGHTING_EXTRA)
                .with_context(|| format!("{path}.extra: expected {} hex bytes", LIGHTING_LEN - LIGHTING_EXTRA))?;
            out[LIGHTING_EXTRA..].copy_from_slice(&bytes);
        }
    }

    b[OFF_MACROS..OFF_MACROS + MACRO_SLOTS * MACRO_LEN].fill(0);
    for (slot, steps) in &cfg.macros {
        let m = slot
            .parse()
            .ok()
            .filter(|m: &usize| (1..=MACRO_SLOTS).contains(m) && m.to_string() == *slot)
            .with_context(|| format!("macros.{slot}: slot must be 1..={MACRO_SLOTS}"))?
            - 1;
        // The device stores an empty macro as an unused slot, so it would not read back.
        ensure!(!steps.is_empty(), "macros.{slot}: a macro needs at least one step");
        let enc = encode_steps(steps).with_context(|| format!("macros.{slot}"))?;
        let out = &mut b[OFF_MACROS + m * MACRO_LEN..][..MACRO_LEN];
        out[0] = enc.len() as u8;
        for (i, s) in enc.iter().enumerate() {
            out[1 + i * 4..][..4].copy_from_slice(s);
        }
    }

    let inner = crc32(&b[OFF_ACTIONS..]);
    b[OFF_INNER_CRC..OFF_INNER_CRC + 4].copy_from_slice(&inner.to_le_bytes());
    Ok(b)
}

fn resolve(name: Option<&String>, cfg: &Config) -> Result<Option<Action>> {
    let Some(name) = name else { return Ok(None) };
    if name == NONE {
        return Ok(None);
    }
    let a = actions::encode(name)?;
    if let Some(m) = name.strip_prefix("macro.") {
        ensure!(cfg.macros.contains_key(m), "`{name}` has no [macros] entry \"{m}\"");
    }
    if let Some(t) = actions::layer_target(name) {
        ensure!(t < cfg.layers.len(), "`{name}`: there are only {} layers", cfg.layers.len());
    }
    Ok(Some(a))
}

/// Flattens a config into `(path, value)` pairs for diffing.
pub fn flatten(cfg: &Config) -> Vec<(String, String)> {
    let mut v = vec![
        ("timing.double_tap_ms".into(), cfg.timing.double_tap_ms.to_string()),
        ("timing.hold_ms".into(), cfg.timing.hold_ms.to_string()),
    ];
    let show = |s: Option<&String>| s.cloned().unwrap_or_else(|| NONE.into());
    for (p, layer) in cfg.layers.iter().enumerate() {
        for (k, key) in layer.keys.iter().enumerate() {
            let s = key.slots();
            for slot in KEY_SLOTS {
                v.push((format!("layers[{p}].keys[{k}].{slot}"), show(s.get(slot))));
            }
        }
        for slot in KNOB_SLOTS {
            v.push((format!("layers[{p}].knob.{slot}"), show(layer.knob.get(slot))));
        }
        let l = &layer.lighting;
        let lp = format!("layers[{p}].lighting");
        v.push((format!("{lp}.effect"), l.effect.to_string()));
        v.push((format!("{lp}.brightness"), l.brightness.to_string()));
        v.push((format!("{lp}.speed"), l.speed.to_string()));
        v.push((format!("{lp}.color"), l.color.to_uppercase()));
        v.push((format!("{lp}.flags"), l.flags.to_string()));
        v.push((format!("{lp}.extra"), l.extra.clone().unwrap_or_default()));
    }
    for (slot, steps) in &cfg.macros {
        v.push((format!("macros.{slot}"), steps.join(", ")));
    }
    v
}

/// Per-item differences `path: before -> after`, in config order.
pub fn diff(before: &Config, after: &Config) -> Vec<String> {
    let a = flatten(before);
    let b = flatten(after);
    let lookup = |v: &[(String, String)], k: &str| v.iter().find(|(p, _)| p == k).map(|(_, x)| x.clone());
    let absent = "(absent)".to_string();
    let mut out = Vec::new();
    for (path, new) in &b {
        let old = lookup(&a, path).unwrap_or_else(|| absent.clone());
        if &old != new {
            out.push(format!("{path}: {old} -> {new}"));
        }
    }
    for (path, old) in &a {
        if lookup(&b, path).is_none() {
            out.push(format!("{path}: {old} -> {absent}"));
        }
    }
    out
}

/// What writing `after` would change on a device holding `before`.
pub enum Changes {
    /// The device's config is unknown; everything is written.
    Unknown,
    /// Per-item `path: old -> new` lines; empty when the blobs are identical.
    Items(Vec<String>),
}

pub fn changes(before: Option<&Blob>, after: &Blob) -> Result<Changes> {
    let Some(old) = before else { return Ok(Changes::Unknown) };
    if old == after {
        return Ok(Changes::Items(Vec::new()));
    }
    let mut lines = diff(&from_blob(old)?, &from_blob(after)?);
    if lines.is_empty() {
        lines.push("action table: reordered, bindings unchanged".into());
    }
    Ok(Changes::Items(lines))
}

pub fn to_toml(cfg: &Config) -> Result<String> {
    toml::to_string_pretty(cfg).context("serialising config")
}

/// Parses a config; format 1 is migrated to the current format.
pub fn from_toml(text: &str) -> Result<Config> {
    let mut cfg: Config = toml::from_str(text).map_err(|e| anyhow::anyhow!("{e}"))?;
    match cfg.format {
        1 => {
            migrate_v1(&mut cfg);
            cfg.format = FORMAT;
        }
        FORMAT => {}
        n => bail!("format = {n}: this bnk8 reads formats 1 and 2; update bnk8"),
    }
    Ok(cfg)
}

/// Renames format-1 action names ([`actions::legacy_name`]) in bindings and
/// macro steps, and shifts canonical `[macros]` keys to 1-based. Other keys
/// are kept, so [`to_blob`] reports them.
fn migrate_v1(cfg: &mut Config) {
    fn rename(v: &mut String) {
        if let Some(new) = actions::legacy_name(v) {
            *v = new;
        }
    }
    fn rename_slots(s: &mut Slots) {
        for n in KNOB_SLOTS {
            if let Some(v) = s.slot_mut(n).unwrap() {
                rename(v);
            }
        }
    }
    for layer in &mut cfg.layers {
        for key in &mut layer.keys {
            match key {
                Key::Tap(a) => rename(a),
                Key::Slots(s) => rename_slots(s),
            }
        }
        rename_slots(&mut layer.knob);
    }
    let macros = std::mem::take(&mut cfg.macros);
    for (slot, mut steps) in macros {
        for step in &mut steps {
            if let Some((verb @ ("tap" | "press" | "release"), key)) = step.split_once(' ')
                && let Some(new) = actions::legacy_name(key)
            {
                *step = format!("{verb} {new}");
            }
        }
        let slot = match slot.parse::<u64>() {
            Ok(n) if n.to_string() == slot => (n + 1).to_string(),
            _ => slot,
        };
        cfg.macros.insert(slot, steps);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_round_trips_byte_for_byte() {
        let cfg = from_blob(FACTORY).unwrap();
        assert_eq!(&to_blob(&cfg, FACTORY).unwrap(), FACTORY);
        // And through TOML text.
        let text = to_toml(&cfg).unwrap();
        let back = from_toml(&text).unwrap();
        assert_eq!(back, cfg);
        assert_eq!(&to_blob(&back, FACTORY).unwrap(), FACTORY);
    }

    #[test]
    fn factory_decodes_known_bindings() {
        let cfg = from_blob(FACTORY).unwrap();
        assert_eq!(cfg.layers.len(), 4);
        let media = &cfg.layers[2];
        assert_eq!(media.knob.cw.as_deref(), Some("media.volume_up"));
        assert_eq!(media.knob.ccw.as_deref(), Some("media.volume_down"));
        assert_eq!(media.knob.tap.as_deref(), Some("media.play_pause"));
        assert_eq!(media.knob.hold.as_deref(), Some("layer.next"));
        assert_eq!(media.keys[5], Key::Tap("media.mute".into()));
        assert_eq!(media.lighting.color, "#FF2300");
    }

    #[test]
    fn set_key_changes_only_that_binding() {
        let mut cfg = from_blob(FACTORY).unwrap();
        cfg.layers[0].keys[0] = Key::Tap("keyboard.a".into());
        let blob = to_blob(&cfg, FACTORY).unwrap();
        let back = from_blob(&blob).unwrap();
        assert_eq!(
            diff(&from_blob(FACTORY).unwrap(), &back),
            ["layers[0].keys[0].tap: codex.task.previous -> keyboard.a"]
        );
        assert_eq!(crc32(&blob[OFF_ACTIONS..]), u32::from_le_bytes(blob[0x14..0x18].try_into().unwrap()));
    }

    #[test]
    fn macros_encode_with_delays_and_limits() {
        let steps: Vec<String> = ["press modifier.left_gui", "delay 50", "tap keyboard.space", "release modifier.left_gui"]
            .map(String::from)
            .into();
        let enc = encode_steps(&steps).unwrap();
        assert_eq!(enc, [[1, 0xe3, 0, 0], [0, 0x2c, 0, 5], [2, 0xe3, 0, 0]]);
        let dec: Vec<String> = enc.iter().flat_map(|s| decode_step(*s)).collect();
        assert_eq!(dec, steps);
        let eight = vec!["tap keyboard.a".to_string(); 8];
        assert!(encode_steps(&eight).is_err());
        assert!(encode_steps(&["delay 15".into(), "tap keyboard.a".into()]).is_err());
    }

    #[test]
    fn validation_errors_carry_paths() {
        let mut cfg = from_blob(FACTORY).unwrap();
        cfg.layers[1].keys[3] = Key::Tap("macro.4".into());
        let e = format!("{:#}", to_blob(&cfg, FACTORY).unwrap_err());
        assert!(e.starts_with("layers[1].keys[3].tap"), "{e}");
        cfg.layers[1].keys.pop();
        let e = to_blob(&cfg, FACTORY).unwrap_err().to_string();
        assert!(e.starts_with("layers[1].keys: 7 keys"), "{e}");
    }

    /// Configs the device would store but not read back the same way.
    #[test]
    fn rejects_configs_that_would_not_round_trip() {
        let base = from_blob(FACTORY).unwrap();
        let err = |cfg: &Config| format!("{:#}", to_blob(cfg, FACTORY).unwrap_err());

        let mut cfg = base.clone();
        cfg.macros.insert("3".into(), vec![]);
        assert!(err(&cfg).contains("macros.3: a macro needs at least one step"), "{}", err(&cfg));

        let mut cfg = base.clone();
        cfg.macros.insert("03".into(), vec!["tap keyboard.a".into()]);
        assert!(err(&cfg).contains("macros.03: slot must be"), "{}", err(&cfg));

        let mut cfg = base.clone();
        cfg.macros.insert("3".into(), vec!["tap keyboard.a".into()]);
        cfg.layers[0].keys[0] = Key::Tap("macro.03".into());
        assert!(err(&cfg).contains("unknown action `macro.03`"), "{}", err(&cfg));

        let mut cfg = base.clone();
        cfg.layers[0].keys[0] = Key::Tap(format!("layer.{}", cfg.layers.len() + 1));
        assert!(err(&cfg).contains("there are only 4 layers"), "{}", err(&cfg));
    }

    const V1: &str = r##"
device = "binepad.bnk8"
[timing]
double_tap_ms = 260
hold_ms = 350
[[layers]]
keys = ["keyboard.up", "macro.0", "layer.0", "none", "none", "none", "none", { hold = "keyboard.dot" }]
[layers.knob]
tap = "layer.next"
cw = "rgb.effect.prev"
[layers.lighting]
effect = 1
brightness = 80
speed = 120
color = "#730000"
flags = 1
[macros]
0 = ["press keyboard.left_ctrl", "delay 10", "tap keyboard.q", "release keyboard.left_ctrl"]
"##;

    #[test]
    fn format_1_is_migrated() {
        let v2 = V1
            .replace("device", "format = 2\ndevice")
            .replace("keyboard.up", "keyboard.arrow_up")
            .replace("macro.0", "macro.1")
            .replace("layer.0", "layer.1")
            .replace("keyboard.dot", "keyboard.period")
            .replace("rgb.effect.prev", "rgb.effect.previous")
            .replace("keyboard.left_ctrl", "modifier.left_control")
            .replace("\n0 = ", "\n1 = ");
        let old = from_toml(V1).unwrap();
        let new = from_toml(&v2).unwrap();
        assert_eq!(old, new);
        assert_eq!(old.layers[0].keys[0], Key::Tap("keyboard.arrow_up".into()));
        assert_eq!(old.macros["1"][0], "press modifier.left_control");
        assert_eq!(to_blob(&old, FACTORY).unwrap(), to_blob(&new, FACTORY).unwrap());
        let e = from_toml(&v2.replace("format = 2", "format = 3")).unwrap_err().to_string();
        assert!(e.contains("format = 3"), "{e}");
    }
}
