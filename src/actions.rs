//! Action codec: dotted action names <-> 7-byte action-table entries.
//!
//! Wire entry (PROTOCOL.md "Action table"): `[id u32][param u16][kind u8]`.
//! `id` is the FNV-1a-32 hash of Binepad Studio's dotted action name. For
//! `kind` 0 the firmware dispatches on `param` alone; kind 1 (desktop) and
//! kind 2 (Claude/Codex) actions are run by Studio's agent, and are named here
//! from Studio's integration packs. Entries this codec cannot name (unknown
//! ids or params, or a non-canonical id/param pairing) decode to
//! `raw.<14 hex>` and encode back byte-for-byte.

use anyhow::{Result, bail};

use crate::studio_ids::IDS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Action {
    pub id: u32,
    pub param: u16,
    pub kind: u8,
}

pub const ENTRY_LEN: usize = 7;

impl Action {
    pub fn from_bytes(b: &[u8; ENTRY_LEN]) -> Self {
        Action {
            id: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            param: u16::from_le_bytes([b[4], b[5]]),
            kind: b[6],
        }
    }

    pub fn to_bytes(self) -> [u8; ENTRY_LEN] {
        let mut b = [0u8; ENTRY_LEN];
        b[..4].copy_from_slice(&self.id.to_le_bytes());
        b[4..6].copy_from_slice(&self.param.to_le_bytes());
        b[6] = self.kind;
        b
    }
}

/// FNV-1a 32-bit, the hash Binepad uses for action ids.
pub fn fnv1a(s: &str) -> u32 {
    s.bytes()
        .fold(0x811c_9dc5, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193))
}

/// Macro slots addressable by `macro.N`, N = 1..=15 (param `0x3000 + N-1`).
pub const MACRO_SLOTS: usize = 15;
/// Layers addressable by `layer.N`, `layer.mo.N` and `layer.tg.N`, N = 1..=10
/// (params `0x2100`, `0x2300`, `0x2310` + N-1).
pub const MAX_LAYERS: usize = 10;

/// Keyboard names by HID usage (QMK basic keycodes 0x04..=0x73, 0x9C).
const KEYBOARD: &[(u16, &str)] = &[
    (0x04, "a"), (0x05, "b"), (0x06, "c"), (0x07, "d"), (0x08, "e"), (0x09, "f"),
    (0x0a, "g"), (0x0b, "h"), (0x0c, "i"), (0x0d, "j"), (0x0e, "k"), (0x0f, "l"),
    (0x10, "m"), (0x11, "n"), (0x12, "o"), (0x13, "p"), (0x14, "q"), (0x15, "r"),
    (0x16, "s"), (0x17, "t"), (0x18, "u"), (0x19, "v"), (0x1a, "w"), (0x1b, "x"),
    (0x1c, "y"), (0x1d, "z"),
    (0x1e, "1"), (0x1f, "2"), (0x20, "3"), (0x21, "4"), (0x22, "5"), (0x23, "6"),
    (0x24, "7"), (0x25, "8"), (0x26, "9"), (0x27, "0"),
    (0x28, "enter"), (0x29, "escape"), (0x2a, "backspace"), (0x2b, "tab"),
    (0x2c, "space"), (0x2d, "minus"), (0x2e, "equal"), (0x2f, "left_bracket"),
    (0x30, "right_bracket"), (0x31, "backslash"), (0x32, "non_us_hash"),
    (0x33, "semicolon"), (0x34, "quote"), (0x35, "grave"), (0x36, "comma"),
    (0x37, "period"), (0x38, "slash"), (0x39, "caps_lock"),
    (0x3a, "f1"), (0x3b, "f2"), (0x3c, "f3"), (0x3d, "f4"), (0x3e, "f5"), (0x3f, "f6"),
    (0x40, "f7"), (0x41, "f8"), (0x42, "f9"), (0x43, "f10"), (0x44, "f11"), (0x45, "f12"),
    (0x46, "print_screen"), (0x47, "scroll_lock"), (0x48, "pause"), (0x49, "insert"),
    (0x4a, "home"), (0x4b, "page_up"), (0x4c, "delete"), (0x4d, "end"),
    (0x4e, "page_down"), (0x4f, "arrow_right"), (0x50, "arrow_left"), (0x51, "arrow_down"),
    (0x52, "arrow_up"),
    (0x53, "num_lock"), (0x54, "numpad.divide"), (0x55, "numpad.multiply"),
    (0x56, "numpad.subtract"), (0x57, "numpad.add"), (0x58, "numpad.enter"),
    (0x59, "numpad.1"), (0x5a, "numpad.2"), (0x5b, "numpad.3"), (0x5c, "numpad.4"),
    (0x5d, "numpad.5"), (0x5e, "numpad.6"), (0x5f, "numpad.7"), (0x60, "numpad.8"),
    (0x61, "numpad.9"), (0x62, "numpad.0"), (0x63, "numpad.decimal"),
    (0x64, "non_us_backslash"), (0x65, "application"), (0x67, "numpad.equal"),
    (0x68, "f13"), (0x69, "f14"), (0x6a, "f15"), (0x6b, "f16"), (0x6c, "f17"),
    (0x6d, "f18"), (0x6e, "f19"), (0x6f, "f20"), (0x70, "f21"), (0x71, "f22"),
    (0x72, "f23"), (0x73, "f24"),
    (0x9c, "numpad.clear"),
];

/// Modifier names (`modifier.*`) by HID usage 0xE0..=0xE7.
const MODIFIERS: &[(u16, &str)] = &[
    (0xe0, "left_control"), (0xe1, "left_shift"), (0xe2, "left_alt"), (0xe3, "left_gui"),
    (0xe4, "right_control"), (0xe5, "right_shift"), (0xe6, "right_alt"), (0xe7, "right_gui"),
];

/// Device-side actions with fixed params (kind 0). `rgb.effect.rainbow`
/// (`0x6012`, BNK9/KnobX1 in Studio) and `wireless.btN` (wireless products)
/// are left out and decode as `raw.*`.
const FIXED: &[(u16, &str)] = &[
    (0x1001, "media.volume_up"),
    (0x1002, "media.volume_down"),
    (0x1003, "media.mute"),
    (0x1004, "media.play_pause"),
    (0x1005, "media.next"),
    (0x1006, "media.previous"),
    (0x2001, "layer.next"),
    (0x2002, "layer.previous"),
    (0x6000, "rgb.toggle"),
    (0x6001, "rgb.on"),
    (0x6002, "rgb.off"),
    (0x6003, "rgb.brightness.up"),
    (0x6004, "rgb.brightness.down"),
    (0x6005, "rgb.hue.up"),
    (0x6006, "rgb.hue.down"),
    (0x6007, "rgb.saturation.up"),
    (0x6008, "rgb.saturation.down"),
    (0x6009, "rgb.effect.next"),
    (0x600a, "rgb.effect.previous"),
    (0x600b, "rgb.speed.up"),
    (0x600c, "rgb.speed.down"),
    (0x600d, "rgb.restore"),
    (0x600e, "rgb.effect.solid"),
    (0x600f, "rgb.effect.breathing"),
    (0x6010, "rgb.effect.wave"),
    (0x6011, "rgb.effect.reactive"),
];

/// Numbered kind-0 actions: name prefix, first param, count. `layer.mo.` and
/// `layer.tg.` come before `layer.`, which would otherwise claim them.
const NUMBERED: &[(&str, u16, usize)] = &[
    ("layer.mo.", 0x2300, MAX_LAYERS),
    ("layer.tg.", 0x2310, MAX_LAYERS),
    ("layer.", 0x2100, MAX_LAYERS),
    ("macro.", 0x3000, MACRO_SLOTS),
];

/// Native macOS params of kind-1 desktop actions, which the firmware can run
/// without Studio's agent. Windows params are not named, so encoding does not
/// depend on the host.
const MAC_NATIVE: &[(&str, u16)] = &[
    ("system.browser.back", 0x1204),
    ("system.browser.forward", 0x1205),
    ("system.browser.refresh", 0x1206),
    ("system.browser.search", 0x1208),
    ("system.edit.copy", 0x1209),
    ("system.edit.paste", 0x120a),
    ("system.edit.cut", 0x120b),
    ("system.edit.undo", 0x120c),
    ("system.edit.redo", 0x120d),
    ("system.edit.select-all", 0x120e),
    ("system.edit.find", 0x120f),
    ("system.window.next", 0x1212),
    ("system.lock", 0x1215),
    ("system.screenshot", 0x1216),
    ("system.window.minimize", 0x1217),
];

/// `rest` as a canonical number in 1..=max: `macro.03` or `layer.+1` would
/// alias another slot.
fn number(rest: &str, max: usize) -> Option<usize> {
    rest.parse::<usize>().ok().filter(|n| n.to_string() == rest && (1..=max).contains(n))
}

/// Canonical name for a kind-0 param, if this codec knows it.
fn param_name(param: u16) -> Option<String> {
    if let Some((_, n)) = KEYBOARD.iter().find(|(p, _)| *p == param) {
        return Some(format!("keyboard.{n}"));
    }
    if let Some((_, n)) = MODIFIERS.iter().find(|(p, _)| *p == param) {
        return Some(format!("modifier.{n}"));
    }
    if let Some((_, n)) = FIXED.iter().find(|(p, _)| *p == param) {
        return Some((*n).to_string());
    }
    NUMBERED.iter().find_map(|&(prefix, first, count)| {
        let i = param.checked_sub(first)? as usize;
        (i < count).then(|| format!("{prefix}{}", i + 1))
    })
}

fn name_param(name: &str) -> Option<u16> {
    if let Some(key) = name.strip_prefix("keyboard.") {
        return KEYBOARD.iter().find(|(_, n)| *n == key).map(|(p, _)| *p);
    }
    if let Some(key) = name.strip_prefix("modifier.") {
        return MODIFIERS.iter().find(|(_, n)| *n == key).map(|(p, _)| *p);
    }
    if let Some((p, _)) = FIXED.iter().find(|(_, n)| *n == name) {
        return Some(*p);
    }
    NUMBERED.iter().find_map(|&(prefix, first, count)| {
        let n = number(name.strip_prefix(prefix)?, count)?;
        Some(first + (n - 1) as u16)
    })
}

/// Kind-1/2 entry for a Studio integration action id.
fn studio_action(name: &str) -> Option<Action> {
    if !IDS.contains(&name) {
        return None;
    }
    let (kind, param) = if name.starts_with("system.") {
        (1, MAC_NATIVE.iter().find(|(n, _)| *n == name).map_or(0, |(_, p)| *p))
    } else {
        (2, 0)
    };
    Some(Action { id: fnv1a(name), param, kind })
}

/// Decodes a string of hex digit pairs; `None` on odd length or any non-hex
/// character (`from_str_radix` alone would accept a `+` sign).
pub(crate) fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

/// Encodes a dotted action name. `none` is handled by the caller (keymap index 0).
pub fn encode(name: &str) -> Result<Action> {
    if let Some(hex) = name.strip_prefix("raw.") {
        let Some(b) = unhex(hex).and_then(|v| <[u8; ENTRY_LEN]>::try_from(v).ok()) else {
            bail!("`{name}`: raw actions are `raw.` + 14 hex digits");
        };
        let a = Action::from_bytes(&b);
        if a.kind > 2 {
            bail!("`{name}`: action kind {} out of range 0..=2", a.kind);
        }
        return Ok(a);
    }
    if let Some(a) = studio_action(name) {
        return Ok(a);
    }
    match name_param(name) {
        Some(param) => Ok(Action { id: fnv1a(name), param, kind: 0 }),
        None => bail!("unknown action `{name}`"),
    }
}

/// Decodes an entry to its canonical name, or `raw.<hex>` when it is not
/// exactly what [`encode`] would produce for a known name.
pub fn decode(a: Action) -> String {
    let name = match a.kind {
        0 => param_name(a.param).filter(|n| fnv1a(n) == a.id),
        1 | 2 => IDS
            .iter()
            .find(|n| fnv1a(n) == a.id)
            .filter(|n| studio_action(n) == Some(a))
            .map(|n| (*n).to_string()),
        _ => None,
    };
    name.unwrap_or_else(|| {
        let hex: String = a.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        format!("raw.{hex}")
    })
}

/// Macro step key codes: keyboard, modifier and media keys (kind-0 params < 0x2000).
pub fn encode_key(name: &str) -> Result<u16> {
    match name_param(name) {
        Some(p) if p < 0x2000 => Ok(p),
        _ => bail!("`{name}` is not a keyboard.*, modifier.* or media.* key"),
    }
}

/// Name of a macro step key code, if nameable.
pub fn key_name(code: u16) -> Option<String> {
    param_name(code).filter(|_| code < 0x2000)
}

/// The prefix and 0-based target layer of a canonical `layer.N`,
/// `layer.mo.N` or `layer.tg.N`.
fn layer_parts(name: &str) -> Option<(&'static str, usize)> {
    NUMBERED[..3].iter().find_map(|&(prefix, _, count)| Some((prefix, number(name.strip_prefix(prefix)?, count)? - 1)))
}

/// 0-based layer a canonical `layer.N`, `layer.mo.N` or `layer.tg.N` targets.
pub fn layer_target(name: &str) -> Option<usize> {
    layer_parts(name).map(|(_, i)| i)
}

/// `name` retargeted at 0-based layer `index`, keeping its kind
/// (`layer.`, `layer.mo.`, `layer.tg.`); `None` when `name` targets no layer.
pub fn with_layer_target(name: &str, index: usize) -> Option<String> {
    layer_parts(name).map(|(prefix, _)| format!("{prefix}{}", index + 1))
}

/// The format-2 name of a format-1 (bnk8 before Studio's names) action name;
/// `None` when it is unchanged. Numbered `layer.N`/`macro.N` shift to 1-based
/// regardless of range, so an invalid old number stays invalid.
pub fn legacy_name(old: &str) -> Option<String> {
    if let Some(k) = old.strip_prefix("keyboard.") {
        let new = match k {
            "up" | "down" | "left" | "right" => format!("keyboard.arrow_{k}"),
            "dot" => "keyboard.period".into(),
            "left_ctrl" => "modifier.left_control".into(),
            "right_ctrl" => "modifier.right_control".into(),
            "left_shift" | "left_alt" | "left_gui" | "right_shift" | "right_alt" | "right_gui" => {
                format!("modifier.{k}")
            }
            _ => return None,
        };
        return Some(new);
    }
    if old == "rgb.effect.prev" {
        return Some("rgb.effect.previous".into());
    }
    for prefix in ["layer.", "macro."] {
        if let Some(rest) = old.strip_prefix(prefix)
            && let Ok(n) = rest.parse::<u64>()
            && n.to_string() == rest
            && let Some(next) = n.checked_add(1)
        {
            return Some(format!("{prefix}{next}"));
        }
    }
    None
}

/// Every name [`encode`] accepts except `raw.*`, in display order: keyboard,
/// modifier, media, layer, rgb, macro, then Studio's integration actions.
pub fn catalog() -> Vec<String> {
    let fixed = |prefix: &'static str| FIXED.iter().filter(move |(_, n)| n.starts_with(prefix)).map(|(_, n)| (*n).to_string());
    let numbered = |prefix: &'static str, count: usize| (1..=count).map(move |n| format!("{prefix}{n}"));
    let mut v: Vec<String> = KEYBOARD.iter().map(|(_, n)| format!("keyboard.{n}")).collect();
    v.extend(MODIFIERS.iter().map(|(_, n)| format!("modifier.{n}")));
    v.extend(fixed("media."));
    v.extend(fixed("layer."));
    v.extend(numbered("layer.", MAX_LAYERS));
    v.extend(numbered("layer.mo.", MAX_LAYERS));
    v.extend(numbered("layer.tg.", MAX_LAYERS));
    v.extend(fixed("rgb."));
    v.extend(numbered("macro.", MACRO_SLOTS));
    v.extend(IDS.iter().map(|n| (*n).to_string()));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_match_factory_config() {
        // Factory action-table entries 25 and 19, byte-for-byte.
        let a = encode("media.volume_up").unwrap();
        assert_eq!(a.to_bytes(), [0x17, 0x5c, 0x81, 0xfa, 0x01, 0x10, 0x00]);
        assert_eq!(decode(a), "media.volume_up");
        let a = encode("layer.next").unwrap();
        assert_eq!(a.to_bytes(), [0x19, 0xbf, 0x67, 0x69, 0x01, 0x20, 0x00]);
    }

    #[test]
    fn unhex_rejects_signs_and_odd_lengths() {
        assert_eq!(unhex("0aFf"), Some(vec![0x0a, 0xff]));
        assert_eq!(unhex("+1"), None, "from_str_radix alone accepts a sign");
        assert_eq!(unhex("abc"), None);
        assert!(encode("raw.+1020304050607").is_err());
    }

    #[test]
    fn every_name_round_trips() {
        for (p, _) in KEYBOARD.iter().chain(MODIFIERS).chain(FIXED) {
            let name = param_name(*p).unwrap();
            assert_eq!(decode(encode(&name).unwrap()), name);
        }
        for n in ["layer.1", "layer.10", "layer.mo.1", "layer.tg.10", "macro.1", "macro.15"] {
            assert_eq!(decode(encode(n).unwrap()), n);
        }
        assert_eq!(encode("layer.mo.3").unwrap().param, 0x2302);
        for n in ["layer.0", "layer.11", "macro.0", "macro.16", "layer.mo.0", "layer.tg.11"] {
            assert!(encode(n).is_err(), "{n}");
        }
    }

    #[test]
    fn studio_integration_actions_are_named() {
        assert_eq!(encode("claude.accept").unwrap().to_bytes(), [0xdf, 0xdd, 0xf9, 0xe8, 0, 0, 2]);
        let copy = encode("system.edit.copy").unwrap();
        assert_eq!((copy.param, copy.kind), (0x1209, 1));
        for name in IDS {
            assert_eq!(decode(encode(name).unwrap()), *name);
        }
        let mut ids: Vec<u32> = IDS.iter().map(|n| fnv1a(n)).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), IDS.len(), "FNV-1a collision among Studio ids");
        // A Studio id with another param (e.g. Windows' native one) stays raw.
        let windows_copy = Action { param: 0x1109, ..copy };
        assert!(decode(windows_copy).starts_with("raw."));
    }

    #[test]
    fn layer_targets() {
        assert_eq!(layer_target("layer.1"), Some(0));
        assert_eq!(layer_target("layer.tg.10"), Some(9));
        assert_eq!(layer_target("layer.next"), None);
        assert_eq!(layer_target("layer.02"), None);
        assert_eq!(with_layer_target("layer.mo.3", 1).as_deref(), Some("layer.mo.2"));
        assert_eq!(with_layer_target("macro.3", 1), None);
    }

    #[test]
    fn unknown_entries_are_raw_and_lossless() {
        let host = Action::from_bytes(&[0x01, 0x02, 0x03, 0x04, 0, 0, 2]);
        let name = decode(host);
        assert_eq!(name, "raw.01020304000002");
        assert_eq!(encode(&name).unwrap(), host);
        // Known param but foreign id stays raw, so Studio's id survives.
        let foreign = Action { id: 1, param: 0x1001, kind: 0 };
        assert_eq!(encode(&decode(foreign)).unwrap(), foreign);
    }
}
