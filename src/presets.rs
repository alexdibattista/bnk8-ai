//! Ready-made layers for video-call apps.
//!
//! Every key emits one keycode, so modifier shortcuts (Cmd+Shift+A) are
//! played as macros. Adding a preset takes free macro slots (reusing a
//! macro with identical steps), and keeps the factory convention of
//! knob hold = `layer.next` so the user can always switch layers.

use std::fmt;

use anyhow::{Result, bail, ensure};

use crate::actions::{MACRO_SLOTS, MAX_LAYERS};
use crate::config::{self, Config, FACTORY, Key, Layer, Lighting, Slots};

/// What a preset key does.
#[derive(Debug, PartialEq)]
pub enum Act {
    /// An action name, e.g. `keyboard.c`.
    Action(&'static str),
    /// Modifiers then a key, as full names (`modifier.left_gui`, `keyboard.a`); played as a macro.
    Combo(&'static [&'static str]),
}

/// A preset key binding.
#[derive(Debug, PartialEq)]
pub enum Bind {
    /// Fires on tap.
    Tap(Act),
    /// Fires after the hold threshold and is released with the key
    /// (push-to-talk, or guarding a destructive action).
    Hold(Act),
}

#[derive(Debug, PartialEq)]
pub struct Preset {
    pub name: &'static str,
    /// What K1..K8 do, for people.
    pub legend: [&'static str; 8],
    keys: [Bind; 8],
    /// Knob press; turning is always volume and hold is always `layer.next`.
    knob_press: Act,
    color: &'static str,
}

impl fmt::Display for Preset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name)
    }
}

use Act::{Action, Combo};
use Bind::{Hold, Tap};

const ZOOM_BLUE: &str = "#0B5CFF";
const MEET_GREEN: &str = "#00AC47";
const ZOOM_LEGEND: [&str; 8] =
    ["Mute mic", "Camera", "Share screen", "Raise hand", "Chat", "Push to talk (hold)", "Mute speakers", "Leave (hold)"];
const MEET_LEGEND: [&str; 8] =
    ["Mute mic", "Camera", "Present", "Raise hand", "Chat", "Push to talk (hold)", "Captions", "Participants"];

/// Zoom desktop client shortcuts; Meet's from Google's "Meet keyboard shortcuts".
/// Meet push to talk must be enabled in Meet's settings.
pub const PRESETS: &[Preset] = &[
    Preset {
        name: "Zoom (macOS)",
        legend: ZOOM_LEGEND,
        keys: [
            Tap(Combo(&["modifier.left_gui", "modifier.left_shift", "keyboard.a"])),
            Tap(Combo(&["modifier.left_gui", "modifier.left_shift", "keyboard.v"])),
            Tap(Combo(&["modifier.left_gui", "modifier.left_shift", "keyboard.s"])),
            Tap(Combo(&["modifier.left_alt", "keyboard.y"])),
            Tap(Combo(&["modifier.left_gui", "modifier.left_shift", "keyboard.h"])),
            Hold(Action("keyboard.space")),
            Tap(Action("media.mute")),
            Hold(Combo(&["modifier.left_gui", "keyboard.w"])),
        ],
        knob_press: Combo(&["modifier.left_gui", "modifier.left_shift", "keyboard.a"]),
        color: ZOOM_BLUE,
    },
    Preset {
        name: "Zoom (Windows/Linux)",
        legend: ZOOM_LEGEND,
        keys: [
            Tap(Combo(&["modifier.left_alt", "keyboard.a"])),
            Tap(Combo(&["modifier.left_alt", "keyboard.v"])),
            Tap(Combo(&["modifier.left_alt", "keyboard.s"])),
            Tap(Combo(&["modifier.left_alt", "keyboard.y"])),
            Tap(Combo(&["modifier.left_alt", "keyboard.h"])),
            Hold(Action("keyboard.space")),
            Tap(Action("media.mute")),
            Hold(Combo(&["modifier.left_alt", "keyboard.q"])),
        ],
        knob_press: Combo(&["modifier.left_alt", "keyboard.a"]),
        color: ZOOM_BLUE,
    },
    Preset {
        name: "Google Meet (macOS)",
        legend: MEET_LEGEND,
        keys: [
            Tap(Combo(&["modifier.left_gui", "keyboard.d"])),
            Tap(Combo(&["modifier.left_gui", "keyboard.e"])),
            Tap(Combo(&["modifier.left_control", "modifier.left_gui", "keyboard.t"])),
            Tap(Combo(&["modifier.left_control", "modifier.left_gui", "keyboard.h"])),
            Tap(Combo(&["modifier.left_control", "modifier.left_gui", "keyboard.c"])),
            Hold(Action("keyboard.space")),
            Tap(Action("keyboard.c")),
            Tap(Combo(&["modifier.left_control", "modifier.left_gui", "keyboard.p"])),
        ],
        knob_press: Combo(&["modifier.left_gui", "keyboard.d"]),
        color: MEET_GREEN,
    },
    Preset {
        name: "Google Meet (Windows/Linux)",
        legend: MEET_LEGEND,
        keys: [
            Tap(Combo(&["modifier.left_control", "keyboard.d"])),
            Tap(Combo(&["modifier.left_control", "keyboard.e"])),
            Tap(Combo(&["modifier.left_control", "modifier.left_alt", "keyboard.t"])),
            Tap(Combo(&["modifier.left_control", "modifier.left_alt", "keyboard.h"])),
            Tap(Combo(&["modifier.left_control", "modifier.left_alt", "keyboard.c"])),
            Hold(Action("keyboard.space")),
            Tap(Action("keyboard.c")),
            Tap(Combo(&["modifier.left_control", "modifier.left_alt", "keyboard.p"])),
        ],
        knob_press: Combo(&["modifier.left_control", "keyboard.d"]),
        color: MEET_GREEN,
    },
];

/// Macro steps for a shortcut: press each modifier, tap the key, release in reverse.
fn combo_steps(keys: &[&str]) -> Vec<String> {
    let (key, mods) = keys.split_last().expect("combo has a key");
    let mut steps: Vec<String> = mods.iter().map(|m| format!("press {m}")).collect();
    steps.push(format!("tap {key}"));
    steps.extend(mods.iter().rev().map(|m| format!("release {m}")));
    steps
}

/// Appends `preset` as a new layer lit like `lighting` in the preset's colour,
/// storing its shortcuts in free macro slots. Returns the new layer's index.
/// On error `cfg` is unchanged.
pub fn add(cfg: &mut Config, preset: &Preset, lighting: &Lighting) -> Result<usize> {
    ensure!(cfg.layers.len() < MAX_LAYERS, "the device holds at most {MAX_LAYERS} layers");
    let mut next = cfg.clone();
    let mut free = (0..MACRO_SLOTS).map(config::macro_key).filter(|k| !cfg.macros.contains_key(k));
    let mut needed = 0;
    let mut resolve = |act: &Act, next: &mut Config| -> Option<String> {
        let steps = match act {
            Action(name) => return Some((*name).to_string()),
            Combo(keys) => combo_steps(keys),
        };
        if let Some((slot, _)) = next.macros.iter().find(|(_, m)| m.steps == steps) {
            return Some(format!("macro.{slot}"));
        }
        needed += 1;
        let slot = free.next()?;
        next.macros.insert(slot.clone(), steps.into());
        Some(format!("macro.{slot}"))
    };
    let mut keys = Vec::with_capacity(8);
    for bind in &preset.keys {
        let (act, hold) = match bind {
            Tap(a) => (a, false),
            Hold(a) => (a, true),
        };
        let action = resolve(act, &mut next);
        keys.push(if hold { Slots { hold: action, ..Slots::default() } } else { Slots { tap: action, ..Slots::default() } });
    }
    let knob = Slots {
        tap: resolve(&preset.knob_press, &mut next),
        hold: Some("layer.next".into()),
        ccw: Some("media.volume_down".into()),
        cw: Some("media.volume_up".into()),
        ..Slots::default()
    };
    let free_slots = MACRO_SLOTS - cfg.macros.len();
    if needed > free_slots {
        bail!(
            "{} needs {needed} free macro slots and {free_slots} are free; clear unused macros on the Macros tab first",
            preset.name
        );
    }
    next.layers.push(Layer {
        name: Some(preset.name.into()),
        keys: keys.into_iter().map(Key::from_slots).collect(),
        knob,
        lighting: Lighting { color: preset.color.into(), key_colors: None, extra: None, flags: None, ..lighting.clone() },
    });
    config::to_blob(&next, FACTORY, &config::StudioIds::new())?;
    *cfg = next;
    Ok(cfg.layers.len() - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn factory() -> Config {
        config::from_blob(FACTORY).unwrap()
    }

    #[test]
    fn every_preset_builds_a_valid_config() {
        for p in PRESETS {
            let mut cfg = factory();
            let lighting = cfg.layers[0].lighting.clone();
            let layer = add(&mut cfg, p, &lighting).unwrap_or_else(|e| panic!("{}: {e:#}", p.name));
            let knob = &cfg.layers[layer].knob;
            assert_eq!(knob.hold.as_deref(), Some("layer.next"), "{}: knob hold must switch layers", p.name);
        }
    }

    #[test]
    fn identical_shortcuts_share_a_macro() {
        let mut cfg = factory();
        let lighting = cfg.layers[0].lighting.clone();
        let before = cfg.macros.len();
        add(&mut cfg, &PRESETS[0], &lighting).unwrap();
        let used = cfg.macros.len() - before;
        add(&mut cfg, &PRESETS[0], &lighting).unwrap();
        assert_eq!(cfg.macros.len() - before, used, "re-adding a preset reuses its macros");
        let k1 = cfg.layers.last().unwrap().keys[0].slots().tap;
        assert_eq!(cfg.layers.last().unwrap().knob.tap, k1, "knob press reuses the mute macro");
    }

    #[test]
    fn running_out_of_macro_slots_leaves_the_config_unchanged() {
        let mut cfg = factory();
        for m in 0..MACRO_SLOTS - 2 {
            cfg.macros.entry(config::macro_key(m)).or_insert_with(|| vec![format!("tap keyboard.{}", m % 9 + 1)].into());
        }
        let before = cfg.clone();
        let lighting = cfg.layers[0].lighting.clone();
        let err = add(&mut cfg, &PRESETS[0], &lighting).unwrap_err();
        assert!(format!("{err}").contains("free macro slots"), "{err}");
        assert_eq!(cfg, before);
    }
}
