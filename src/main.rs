mod app;
mod comic;

use cosmic::app::Settings;
use cosmic::iced::Size;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let settings = Settings::default().size(Size::new(1024.0, 768.0));

    cosmic::app::run::<app::App>(settings, ())?;

    Ok(())
}
