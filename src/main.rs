mod app;
mod comic;
mod comicinfo;
mod comicvine;
mod epub;
mod library;
mod metadata;
mod settings;

use cosmic::app::Settings;
use cosmic::iced::Size;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let path = std::env::args().nth(1).map(std::path::PathBuf::from);

    let settings = Settings::default().size(Size::new(1024.0, 768.0));

    cosmic::app::run::<app::App>(settings, path)?;

    Ok(())
}
