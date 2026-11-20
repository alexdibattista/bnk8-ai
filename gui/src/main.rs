//! Desktop configurator for the Binepad BNK8 AI Edition.

mod app;
mod style;
mod widgets;

use iced::{Settings, Size, window};

fn main() -> iced::Result {
    iced::application(app::App::new, app::App::update, app::App::view)
        .title("BNK8 Configurator")
        .theme(style::theme())
        .settings(Settings {
            fonts: style::FONTS.map(Into::into).into(),
            default_font: style::REGULAR,
            default_text_size: 14.into(),
            ..Settings::default()
        })
        .window(window::Settings {
            size: Size::new(1360.0, 860.0),
            min_size: Some(Size::new(960.0, 640.0)),
            ..window::Settings::default()
        })
        .run()
}
