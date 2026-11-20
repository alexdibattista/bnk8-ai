//! Palette and widget styles.

use iced::border::{self, Border};
use iced::theme::Palette;
use iced::widget::{button, container, pick_list, text, text_input};
use iced::{Background, Color, Font, Shadow, Theme, Vector, color, font};

/// Bundled so headings get real bold/semibold faces on every platform.
pub const FONTS: [&[u8]; 3] = [
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Inter-Bold.ttf"),
];
pub const REGULAR: Font = Font::with_name("Inter");
pub const BOLD: Font = Font { weight: font::Weight::Bold, ..REGULAR };
pub const SEMIBOLD: Font = Font { weight: font::Weight::Semibold, ..REGULAR };

const RADIUS: f32 = 8.0;
const KEYCAP: Color = color!(0x252833);
const KEYCAP_HOVER: Color = color!(0x2E3240);
const KEYCAP_PRESSED: Color = color!(0x1F222B);
const DEVICE: Color = color!(0x0B0C10);

pub fn theme() -> Theme {
    Theme::custom(
        "BNK8",
        Palette {
            background: color!(0x121419),
            text: color!(0xE6E8EE),
            primary: color!(0x7C6CF6),
            success: color!(0x3FB980),
            warning: color!(0xE5A93B),
            danger: color!(0xEF5A5A),
        },
    )
}

fn muted_color(theme: &Theme) -> Color {
    theme.palette().text.scale_alpha(0.55)
}

// ---- text ------------------------------------------------------------

pub fn muted(theme: &Theme) -> text::Style {
    text::Style { color: Some(muted_color(theme)) }
}

pub fn section(theme: &Theme) -> text::Style {
    text::Style { color: Some(theme.palette().text.scale_alpha(0.4)) }
}

pub fn accent(theme: &Theme) -> text::Style {
    text::Style { color: Some(theme.palette().primary) }
}

// ---- containers ------------------------------------------------------

pub fn chrome(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(theme.extended_palette().background.weakest.color.into()),
        ..container::Style::default()
    }
}

pub fn card(theme: &Theme) -> container::Style {
    let p = theme.extended_palette();
    container::Style {
        background: Some(p.background.weakest.color.into()),
        border: Border { color: p.background.weak.color, width: 1.0, radius: 14.0.into() },
        ..container::Style::default()
    }
}

pub fn dialog(theme: &Theme) -> container::Style {
    container::Style {
        shadow: Shadow { color: Color::BLACK.scale_alpha(0.5), offset: Vector::new(0.0, 12.0), blur_radius: 40.0 },
        ..card(theme)
    }
}

pub fn backdrop(_theme: &Theme) -> container::Style {
    container::Style { background: Some(Color::BLACK.scale_alpha(0.6).into()), ..container::Style::default() }
}

/// The keypad body; `glow` is the layer's lighting colour, if lit.
pub fn device(glow: Option<Color>) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let edge = glow.map_or(theme.extended_palette().background.weak.color, |c| c.scale_alpha(0.7));
        container::Style {
            background: Some(DEVICE.into()),
            border: Border { color: edge, width: 1.5, radius: 24.0.into() },
            shadow: Shadow {
                color: glow.map_or(Color::BLACK.scale_alpha(0.5), |c| c.scale_alpha(0.35)),
                offset: Vector::ZERO,
                blur_radius: 32.0,
            },
            ..container::Style::default()
        }
    }
}

pub fn swatch(fill: Color) -> impl Fn(&Theme) -> container::Style {
    move |theme| container::Style {
        background: Some(fill.into()),
        border: Border { color: theme.palette().text.scale_alpha(0.2), width: 1.0, radius: 4.0.into() },
        ..container::Style::default()
    }
}

#[derive(Clone, Copy)]
pub enum Tone {
    Neutral,
    Accent,
    Success,
    Danger,
}

impl Tone {
    fn color(self, theme: &Theme) -> Color {
        let p = theme.palette();
        match self {
            Tone::Neutral => muted_color(theme),
            Tone::Accent => p.primary,
            Tone::Success => p.success,
            Tone::Danger => p.danger,
        }
    }
}

pub fn pill(tone: Tone) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let c = tone.color(theme);
        container::Style {
            text_color: Some(c),
            background: Some(c.scale_alpha(0.12).into()),
            border: border::rounded(999),
            ..container::Style::default()
        }
    }
}

pub fn banner(tone: Tone) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let c = tone.color(theme);
        container::Style {
            text_color: Some(theme.palette().text),
            background: Some(c.scale_alpha(0.12).into()),
            border: Border { color: c.scale_alpha(0.45), width: 1.0, radius: RADIUS.into() },
            ..container::Style::default()
        }
    }
}

pub fn dot(tone: Tone) -> impl Fn(&Theme) -> container::Style {
    move |theme| container::Style {
        background: Some(tone.color(theme).into()),
        border: border::rounded(999),
        ..container::Style::default()
    }
}

// ---- buttons ---------------------------------------------------------

fn disabled(style: button::Style) -> button::Style {
    button::Style {
        background: style.background.map(|b| b.scale_alpha(0.5)),
        text_color: style.text_color.scale_alpha(0.4),
        border: Border { color: style.border.color.scale_alpha(0.4), ..style.border },
        ..style
    }
}

/// A key on the keypad mock-up; `radius` rounds it into the knob.
pub fn keycap(selected: bool, radius: f32) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let primary = theme.palette().primary;
        let (fill, lift) = match status {
            button::Status::Hovered => (KEYCAP_HOVER, 4.0),
            button::Status::Pressed => (KEYCAP_PRESSED, 1.0),
            _ => (KEYCAP, 3.0),
        };
        let fill = if selected { mix(fill, primary, 0.22) } else { fill };
        button::Style {
            background: Some(Background::Color(fill)),
            text_color: theme.palette().text,
            border: Border {
                color: if selected { primary } else { Color::WHITE.scale_alpha(0.06) },
                width: if selected { 2.0 } else { 1.0 },
                radius: radius.into(),
            },
            shadow: Shadow { color: Color::BLACK.scale_alpha(0.6), offset: Vector::new(0.0, lift), blur_radius: 2.0 },
            snap: true,
        }
    }
}

/// Sidebar and list entries.
pub fn nav(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let p = theme.extended_palette();
        let background = match (selected, status) {
            (true, _) => Some(p.primary.base.color.scale_alpha(0.18).into()),
            (false, button::Status::Hovered | button::Status::Pressed) => Some(p.background.weak.color.into()),
            _ => None,
        };
        button::Style {
            background,
            text_color: if selected { mix(p.primary.base.color, Color::WHITE, 0.45) } else { p.background.base.text },
            border: border::rounded(RADIUS),
            ..button::Style::default()
        }
    }
}

pub fn ghost(theme: &Theme, status: button::Status) -> button::Style {
    outlined(theme, status, theme.palette().text)
}

pub fn ghost_danger(theme: &Theme, status: button::Status) -> button::Style {
    outlined(theme, status, theme.palette().danger)
}

fn outlined(theme: &Theme, status: button::Status, ink: Color) -> button::Style {
    let p = theme.extended_palette();
    let base = button::Style {
        background: None,
        text_color: ink,
        border: Border { color: p.background.strong.color, width: 1.0, radius: RADIUS.into() },
        ..button::Style::default()
    };
    match status {
        button::Status::Active => base,
        button::Status::Hovered => button::Style { background: Some(p.background.weak.color.into()), ..base },
        button::Status::Pressed => button::Style { background: Some(p.background.strong.color.into()), ..base },
        button::Status::Disabled => disabled(base),
    }
}

/// Borderless icon-sized button.
pub fn icon(theme: &Theme, status: button::Status) -> button::Style {
    let p = theme.extended_palette();
    let base = button::Style {
        background: None,
        text_color: muted_color(theme),
        border: border::rounded(6),
        ..button::Style::default()
    };
    match status {
        button::Status::Active => base,
        button::Status::Hovered | button::Status::Pressed => {
            button::Style { background: Some(p.background.weak.color.into()), text_color: p.background.base.text, ..base }
        }
        button::Status::Disabled => disabled(base),
    }
}

pub fn primary(theme: &Theme, status: button::Status) -> button::Style {
    let style = button::primary(theme, status);
    button::Style { border: Border { radius: RADIUS.into(), ..style.border }, ..style }
}

pub fn danger(theme: &Theme, status: button::Status) -> button::Style {
    let style = button::danger(theme, status);
    button::Style { border: Border { radius: RADIUS.into(), ..style.border }, ..style }
}

// ---- inputs ----------------------------------------------------------

pub fn field(theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let style = pick_list::default(theme, status);
    pick_list::Style { border: Border { radius: RADIUS.into(), ..style.border }, ..style }
}

pub fn input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let style = text_input::default(theme, status);
    text_input::Style { border: Border { radius: RADIUS.into(), ..style.border }, ..style }
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    Color { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: a.a }
}
