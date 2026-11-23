//! Action names for humans, and the action pickers.

use std::fmt;

use iced::widget::{pick_list, row, text, text_input, tooltip};
use iced::{Element, Font};

use bnk8::actions;

use crate::style;

const RAW_ZERO: &str = "raw.00000000000000";
const PICKER_WIDTH: u32 = 200;

/// Declaration order matches [`Category::ALL`] (checked below): it indexes [`Catalog`]'s groups.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    None,
    Keyboard,
    Media,
    Layer,
    Lighting,
    Macro,
    Claude,
    Codex,
    Desktop,
    Raw,
}

impl Category {
    const ALL: [Category; 10] = [
        Category::None,
        Category::Keyboard,
        Category::Media,
        Category::Layer,
        Category::Lighting,
        Category::Macro,
        Category::Claude,
        Category::Codex,
        Category::Desktop,
        Category::Raw,
    ];

    fn of(name: Option<&str>) -> Category {
        let Some(n) = name else { return Category::None };
        match n.split('.').next().unwrap_or("") {
            "keyboard" | "modifier" => Category::Keyboard,
            "media" => Category::Media,
            "layer" => Category::Layer,
            "rgb" => Category::Lighting,
            "macro" => Category::Macro,
            "claude" => Category::Claude,
            "codex" => Category::Codex,
            "system" => Category::Desktop,
            _ => Category::Raw,
        }
    }

    /// Catalog prefixes of the category's entries; empty for categories without a list.
    fn prefixes(self) -> &'static [&'static str] {
        match self {
            Category::Keyboard => &["keyboard.", "modifier."],
            Category::Media => &["media."],
            Category::Layer => &["layer."],
            Category::Lighting => &["rgb."],
            Category::Macro => &["macro."],
            Category::Claude => &["claude."],
            Category::Codex => &["codex."],
            Category::Desktop => &["system."],
            Category::None | Category::Raw => &[],
        }
    }

    fn label(self) -> &'static str {
        match self {
            Category::None => "Nothing",
            Category::Keyboard => "Key",
            Category::Media => "Media",
            Category::Layer => "Layer",
            Category::Lighting => "Lighting",
            Category::Macro => "Macro",
            Category::Claude => "Claude",
            Category::Codex => "Codex",
            Category::Desktop => "Desktop",
            Category::Raw => "Raw / Studio",
        }
    }
}

// `Catalog::group` indexes by `Category as usize`.
const _: () = {
    let mut i = 0;
    while i < Category::ALL.len() {
        assert!(Category::ALL[i] as usize == i);
        i += 1;
    }
};

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A catalog entry in a pick list: shown by its short label, compared by name.
#[derive(Clone, Debug)]
pub struct Choice {
    name: String,
    label: String,
}

impl Choice {
    fn new(name: &str) -> Self {
        Choice { name: name.to_string(), label: short(Some(name)) }
    }
}

impl PartialEq for Choice {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl fmt::Display for Choice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)
    }
}

/// The action catalog, grouped for the pickers.
pub struct Catalog {
    /// Indexed by `Category as usize`; empty for categories without a list.
    groups: Vec<Vec<Choice>>,
    /// Keyboard, modifier and media keys, for macro steps.
    keys: Vec<Choice>,
}

impl Catalog {
    pub fn new() -> Self {
        let names = actions::catalog();
        let entries = |prefixes: &[&str]| -> Vec<Choice> {
            names.iter().filter(|n| prefixes.iter().any(|p| n.starts_with(p))).map(|n| Choice::new(n)).collect()
        };
        Catalog {
            groups: Category::ALL.iter().map(|c| entries(c.prefixes())).collect(),
            keys: entries(&["keyboard.", "modifier.", "media."]),
        }
    }

    fn group(&self, c: Category) -> &[Choice] {
        &self.groups[c as usize]
    }

    /// The binding after switching `from` to category `to`.
    fn switch(&self, from: Option<&str>, to: Category) -> Option<String> {
        if Category::of(from) == to {
            return from.map(str::to_string);
        }
        match to {
            Category::None => None,
            Category::Raw => Some(from.map_or_else(|| RAW_ZERO.to_string(), to_raw)),
            c => self.group(c).first().map(|c| c.name.clone()),
        }
    }
}

/// Short label for a binding, as shown on the keypad buttons.
pub fn short(name: Option<&str>) -> String {
    let Some(n) = name else { return "-".into() };
    if let Some(k) = n.strip_prefix("keyboard.") {
        if let Some(p) = k.strip_prefix("numpad.") {
            return format!("Num {}", pretty(p));
        }
        return if k.chars().count() == 1 { k.to_uppercase() } else { pretty(k) };
    }
    if let Some(m) = n.strip_prefix("modifier.") {
        return pretty(m);
    }
    if let Some(m) = n.strip_prefix("media.") {
        return match m {
            "volume_up" => "Vol +",
            "volume_down" => "Vol -",
            "mute" => "Mute",
            "play_pause" => "Play/Pause",
            "next" => "Next track",
            "previous" => "Prev track",
            _ => m,
        }
        .into();
    }
    if let Some(t) = actions::layer_target(n) {
        let what = if n.starts_with("layer.mo.") {
            "Hold layer"
        } else if n.starts_with("layer.tg.") {
            "Toggle layer"
        } else {
            "Layer"
        };
        return format!("{what} {t}");
    }
    match n {
        "layer.next" => return "Next layer".into(),
        "layer.previous" => return "Prev layer".into(),
        _ => {}
    }
    if let Some(m) = n.strip_prefix("macro.") {
        return format!("Macro {m}");
    }
    if let Some(rest) = ["claude.", "codex.", "system."].iter().find_map(|p| n.strip_prefix(p)) {
        return pretty(&rest.replace(['.', '-'], " "));
    }
    if let Some(r) = n.strip_prefix("rgb.") {
        return format!("RGB {}", r.replace('.', " "));
    }
    if let Some(hex) = n.strip_prefix("raw.") {
        return match hex.get(12..14) {
            Some("02") => "Studio action".into(),
            Some("01") => "Device built-in".into(),
            _ => "Raw".into(),
        };
    }
    n.into()
}

/// A macro as the shortcuts it plays, e.g. `Cmd+Shift+A`; empty when it taps nothing.
pub fn macro_summary(steps: &[String]) -> String {
    let mut held: Vec<String> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for step in steps {
        let Some((verb, key)) = step.split_once(' ') else { continue };
        match verb {
            "press" => held.push(key_label(key)),
            "release" => {
                let k = key_label(key);
                if let Some(i) = held.iter().rposition(|h| *h == k) {
                    held.remove(i);
                }
            }
            "tap" => out.push(held.iter().cloned().chain([key_label(key)]).collect::<Vec<_>>().join("+")),
            _ => {}
        }
    }
    out.join(" ")
}

/// Key name in a shortcut; modifiers use the host's names.
fn key_label(name: &str) -> String {
    let modifier = name.strip_prefix("modifier.").and_then(|k| k.strip_prefix("left_").or(k.strip_prefix("right_")));
    match modifier {
        Some("control") => "Ctrl".into(),
        Some("shift") => "Shift".into(),
        Some("alt") if cfg!(target_os = "macos") => "Opt".into(),
        Some("alt") => "Alt".into(),
        Some("gui") if cfg!(target_os = "macos") => "Cmd".into(),
        Some("gui") => "Win".into(),
        _ => short(Some(name)),
    }
}

/// `snake_case` as a capitalised phrase: `volume_up` -> `Volume up`.
pub fn pretty(s: &str) -> String {
    let s = s.replace('_', " ");
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => s,
    }
}

fn to_raw(name: &str) -> String {
    match actions::encode(name) {
        Ok(a) => format!("raw.{}", a.to_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>()),
        Err(_) => RAW_ZERO.into(),
    }
}

/// Category + item picker for one binding.
pub fn action_picker<'a, M: Clone + 'a>(
    catalog: &'a Catalog,
    value: Option<&'a str>,
    on_change: impl Fn(Option<String>) -> M + Clone + 'a,
) -> Element<'a, M> {
    let cat = Category::of(value);
    let categories = {
        let on_change = on_change.clone();
        pick_list(Category::ALL, Some(cat), move |to| on_change(catalog.switch(value, to)))
            .width(130)
            .style(style::field)
    };
    let item: Option<Element<'a, M>> = match cat {
        Category::None => None,
        Category::Raw => Some(
            tooltip(
                text_input("raw.…", value.unwrap_or_default())
                    .on_input(move |t| on_change(Some(t)))
                    .font(Font::MONOSPACE)
                    .width(PICKER_WIDTH)
                    .style(style::input),
                tip("raw.<id u32 LE><param u16 LE><kind>, 14 hex digits"),
                tooltip::Position::Top,
            )
            .into(),
        ),
        c => Some(
            pick_list(catalog.group(c), value.map(Choice::new), move |ch: Choice| on_change(Some(ch.name)))
                .width(PICKER_WIDTH)
                .menu_height(320)
                .style(style::field)
                .into(),
        ),
    };
    row![categories].push(item).spacing(8).into()
}

/// Picker for macro step keys (keyboard and media only).
pub fn key_picker<'a, M: Clone + 'a>(catalog: &'a Catalog, key: &str, on_change: impl Fn(String) -> M + 'a) -> Element<'a, M> {
    pick_list(&catalog.keys[..], Some(Choice::new(key)), move |ch: Choice| on_change(ch.name))
        .width(PICKER_WIDTH)
        .menu_height(320)
        .style(style::field)
        .into()
}

/// Tooltip body.
pub fn tip<'a, M: 'a>(body: &'a str) -> Element<'a, M> {
    iced::widget::container(text(body).size(12))
        .padding([6, 10])
        .max_width(320)
        .style(style::card)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_readable() {
        assert_eq!(short(Some("keyboard.a")), "A");
        assert_eq!(short(Some("modifier.left_control")), "Left control");
        assert_eq!(short(Some("layer.1")), "Layer 0");
        assert_eq!(short(Some("layer.mo.3")), "Hold layer 2");
        assert_eq!(short(Some("claude.code.session.previous")), "Code session previous");
        assert_eq!(short(Some("keyboard.numpad.7")), "Num 7");
        assert_eq!(short(Some("raw.dfddf9e8000002")), "Studio action");
        assert_eq!(short(None), "-");
    }

    #[test]
    fn raw_conversion_keeps_the_entry() {
        assert_eq!(to_raw("media.volume_up"), "raw.175c81fa011000");
    }

    #[test]
    fn macros_read_as_shortcuts() {
        let steps = |s: &[&str]| s.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let combo = steps(&["press modifier.left_control", "press modifier.left_shift", "tap keyboard.a", "release modifier.left_shift", "release modifier.left_control"]);
        assert_eq!(macro_summary(&combo), "Ctrl+Shift+A");
        let sequence = steps(&["press modifier.left_control", "tap keyboard.c", "release modifier.left_control", "delay 100", "tap keyboard.v"]);
        assert_eq!(macro_summary(&sequence), "Ctrl+C V");
    }
}
