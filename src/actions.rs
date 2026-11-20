//! Action codec: dotted action names <-> 7-byte action-table entries.
//!
//! Wire entry (PROTOCOL.md "Action table"): `[id u32][param u16][kind u8]`.
//! For `kind` 0 the firmware dispatches on `param` alone; `id` is the
//! FNV-1a-32 hash of the dotted name (confirmed for the factory `media.*`
//! and `layer.next` entries) and only matters to Studio. Entries this codec
//! cannot name (host/AI actions, kind 1/2, unknown params, or a non-canonical
//! id) decode to `raw.<14 hex>` and encode back byte-for-byte.

use anyhow::{Result, bail};

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

/// Macro slots addressable by `macro.N` (param `0x3000 | N`).
pub const MACRO_SLOTS: usize = 15;
/// Layers (stored profiles) addressable by `layer.N` (param `0x2100 | N`).
pub const MAX_LAYERS: usize = 10;

/// Keyboard names by HID usage (QMK basic keycodes 0x04..=0x73, 0xE0..=0xE7).
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
    (0x37, "dot"), (0x38, "slash"), (0x39, "caps_lock"),
    (0x3a, "f1"), (0x3b, "f2"), (0x3c, "f3"), (0x3d, "f4"), (0x3e, "f5"), (0x3f, "f6"),
    (0x40, "f7"), (0x41, "f8"), (0x42, "f9"), (0x43, "f10"), (0x44, "f11"), (0x45, "f12"),
    (0x46, "print_screen"), (0x47, "scroll_lock"), (0x48, "pause"), (0x49, "insert"),
    (0x4a, "home"), (0x4b, "page_up"), (0x4c, "delete"), (0x4d, "end"),
    (0x4e, "page_down"), (0x4f, "right"), (0x50, "left"), (0x51, "down"), (0x52, "up"),
    (0x53, "num_lock"), (0x54, "numpad.divide"), (0x55, "numpad.multiply"),
    (0x56, "numpad.subtract"), (0x57, "numpad.add"), (0x58, "numpad.enter"),
    (0x59, "numpad.1"), (0x5a, "numpad.2"), (0x5b, "numpad.3"), (0x5c, "numpad.4"),
    (0x5d, "numpad.5"), (0x5e, "numpad.6"), (0x5f, "numpad.7"), (0x60, "numpad.8"),
    (0x61, "numpad.9"), (0x62, "numpad.0"), (0x63, "numpad.decimal"),
    (0x64, "non_us_backslash"), (0x65, "application"), (0x67, "numpad.equal"),
    (0x68, "f13"), (0x69, "f14"), (0x6a, "f15"), (0x6b, "f16"), (0x6c, "f17"),
    (0x6d, "f18"), (0x6e, "f19"), (0x6f, "f20"), (0x70, "f21"), (0x71, "f22"),
    (0x72, "f23"), (0x73, "f24"),
    (0xe0, "left_ctrl"), (0xe1, "left_shift"), (0xe2, "left_alt"), (0xe3, "left_gui"),
    (0xe4, "right_ctrl"), (0xe5, "right_shift"), (0xe6, "right_alt"), (0xe7, "right_gui"),
];

/// Device-side actions with fixed params (kind 0).
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
    (0x600a, "rgb.effect.prev"),
    (0x600b, "rgb.speed.up"),
    (0x600c, "rgb.speed.down"),
];

/// Canonical name for a kind-0 param, if this codec knows it.
fn param_name(param: u16) -> Option<String> {
    if let Some((_, n)) = KEYBOARD.iter().find(|(p, _)| *p == param) {
        return Some(format!("keyboard.{n}"));
    }
    if let Some((_, n)) = FIXED.iter().find(|(p, _)| *p == param) {
        return Some((*n).to_string());
    }
    match param & 0xff00 {
        0x2100 if (param & 0xff) < MAX_LAYERS as u16 => Some(format!("layer.{}", param & 0xff)),
        0x3000 if (param & 0xff) < MACRO_SLOTS as u16 => Some(format!("macro.{}", param & 0xff)),
        _ => None,
    }
}

fn name_param(name: &str) -> Option<u16> {
    if let Some(key) = name.strip_prefix("keyboard.") {
        return KEYBOARD.iter().find(|(_, n)| *n == key).map(|(p, _)| *p);
    }
    if let Some((p, _)) = FIXED.iter().find(|(_, n)| *n == name) {
        return Some(*p);
    }
    // Canonical numbers only: `macro.03` or `layer.+1` would alias another slot.
    let index = |rest: &str, max: usize| rest.parse::<u16>().ok().filter(|n| n.to_string() == rest && (*n as usize) < max);
    if let Some(rest) = name.strip_prefix("layer.") {
        return index(rest, MAX_LAYERS).map(|n| 0x2100 | n);
    }
    if let Some(rest) = name.strip_prefix("macro.") {
        return index(rest, MACRO_SLOTS).map(|n| 0x3000 | n);
    }
    None
}

/// Encodes a dotted action name. `none` is handled by the caller (keymap index 0).
pub fn encode(name: &str) -> Result<Action> {
    if let Some(hex) = name.strip_prefix("raw.") {
        let ok = hex.len() == ENTRY_LEN * 2 && hex.bytes().all(|c| c.is_ascii_hexdigit());
        if !ok {
            bail!("`{name}`: raw actions are `raw.` + 14 hex digits");
        }
        let mut b = [0u8; ENTRY_LEN];
        for (i, out) in b.iter_mut().enumerate() {
            *out = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
        }
        let a = Action::from_bytes(&b);
        if a.kind > 2 {
            bail!("`{name}`: action kind {} out of range 0..=2", a.kind);
        }
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
    if a.kind == 0
        && let Some(name) = param_name(a.param)
        && fnv1a(&name) == a.id
    {
        return name;
    }
    let hex: String = a.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
    format!("raw.{hex}")
}

/// Macro step key codes: keyboard usages and media keys (kind-0 params < 0x2000).
pub fn encode_key(name: &str) -> Result<u16> {
    match name_param(name) {
        Some(p) if p < 0x2000 => Ok(p),
        _ => bail!("`{name}` is not a keyboard.* or media.* key"),
    }
}

/// Name of a macro step key code, if nameable.
pub fn key_name(code: u16) -> Option<String> {
    param_name(code).filter(|_| code < 0x2000)
}

/// Every name [`encode`] accepts except `raw.*`, in display order:
/// keyboard, media, layer, rgb, macro.
pub fn catalog() -> Vec<String> {
    let mut v: Vec<String> = KEYBOARD.iter().map(|(_, n)| format!("keyboard.{n}")).collect();
    v.extend(FIXED.iter().map(|(_, n)| (*n).to_string()));
    v.extend((0..MAX_LAYERS).map(|n| format!("layer.{n}")));
    v.extend((0..MACRO_SLOTS).map(|n| format!("macro.{n}")));
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
    fn every_name_round_trips() {
        for (p, _) in KEYBOARD.iter().chain(FIXED) {
            let name = param_name(*p).unwrap();
            assert_eq!(decode(encode(&name).unwrap()), name);
        }
        for n in ["layer.0", "layer.9", "macro.0", "macro.14"] {
            assert_eq!(decode(encode(n).unwrap()), n);
        }
        assert!(encode("macro.15").is_err());
        assert!(encode("layer.10").is_err());
    }

    #[test]
    fn unknown_entries_are_raw_and_lossless() {
        let host = Action::from_bytes(&[0xdf, 0xdd, 0xf9, 0xe8, 0, 0, 2]);
        let name = decode(host);
        assert_eq!(name, "raw.dfddf9e8000002");
        assert_eq!(encode(&name).unwrap(), host);
        // Known param but foreign id stays raw, so Studio's id survives.
        let foreign = Action { id: 1, param: 0x1001, kind: 0 };
        assert_eq!(encode(&decode(foreign)).unwrap(), foreign);
    }
}
