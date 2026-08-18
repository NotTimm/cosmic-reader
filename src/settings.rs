//! User settings, persisted as JSON in the XDG config directory.
//!
//! Kept deliberately separate from the library database: settings live in
//! `$XDG_CONFIG_HOME`, the library lives in `$XDG_DATA_HOME`, and derived
//! artifacts (covers, extracted archives) live in `$XDG_CACHE_HOME`. That
//! split is what makes state survive reinstalls and upgrades predictably —
//! a user can clear the cache without losing their library or preferences.

use serde::{Deserialize, Serialize};

/// How the reader fills space around a page that doesn't match the window
/// aspect ratio.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Background {
    /// Follow the COSMIC theme.
    Theme,
    /// Solid black, like a cinema.
    Black,
}

impl Background {
    pub const ALL: [Background; 2] = [Background::Theme, Background::Black];

    pub fn label(self) -> &'static str {
        match self {
            Background::Theme => "Theme",
            Background::Black => "Black",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Opacity of the reader's backdrop (the area around the page), 0.0–1.0.
    /// Applies to the background only, never to the page image itself.
    pub background_opacity: f32,
    pub background: Background,
    /// Restore these reader preferences across sessions.
    pub dual_page: bool,
    pub theater_mode: bool,
    /// Look up metadata online (AniList / ComicVine) when opening a comic.
    pub fetch_metadata: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            background_opacity: 1.0,
            background: Background::Theme,
            dual_page: false,
            theater_mode: false,
            fetch_metadata: true,
        }
    }
}

fn config_path() -> std::path::PathBuf {
    crate::library::config_dir().join("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read(config_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = config_path();
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}
