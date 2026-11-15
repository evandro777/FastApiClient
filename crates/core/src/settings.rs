//! User settings in one TOML file under the XDG data dir (`~/.local/share/FastApiClient`).
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::files::{read_or_backup, write_atomic};

/// Folder name under the XDG data dir; `rename.sh` rewrites it.
pub const APP_DIR: &str = "FastApiClient";
pub const MIN_SIZE: (u32, u32) = (400, 300);
/// Upper bound for a hand-edited size; a huge value would fail at window creation.
pub const MAX_SIZE: u32 = 8192;

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)] // older files keep loading when a field is added
pub struct Settings {
    /// "" follows the system locale; otherwise a gettext code such as "pt_BR".
    pub language: String,
    pub width: u32,
    pub height: u32,
    /// Id of the project that was selected when the app closed; "" for none.
    pub last_project: String,
    pub last_folder: Option<usize>,
    pub last_request: Option<usize>,
    /// Show JSON/XML responses re-indented as soon as they arrive.
    pub auto_beautify: bool,
    /// "" follows the system's light or dark mode; otherwise "light" or "dark".
    pub theme: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: String::new(),
            width: 1000,
            height: 700,
            last_project: String::new(),
            last_folder: None,
            last_request: None,
            auto_beautify: true,
            theme: String::new(),
        }
    }
}

fn parse(text: &str) -> Result<Settings, toml::de::Error> {
    toml::from_str(text)
}

pub(crate) fn data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(APP_DIR)
}

fn path() -> PathBuf {
    data_dir().join("settings.toml")
}

impl Settings {
    pub fn load() -> Self {
        // The file is hand-edited; a bad one is moved aside so the next save() must not overwrite what the user wrote.
        let mut s = read_or_backup(&path(), |t| parse(t).map_err(|e| e.to_string())).0;
        s.normalize();
        s
    }

    fn normalize(&mut self) {
        self.width = self.width.clamp(MIN_SIZE.0, MAX_SIZE);
        self.height = self.height.clamp(MIN_SIZE.1, MAX_SIZE);
    }

    /// Atomic write so a crash can never leave a half-written file. Failures are reported on stderr;
    /// the app keeps working with the in-memory settings.
    pub fn save(&self) {
        if let Err(e) = self.try_save() {
            eprintln!("cannot save settings to {}: {e}", path().display());
        }
    }

    fn try_save(&self) -> std::io::Result<()> {
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        write_atomic(&path(), text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_files_still_load() {
        let parsed: Settings = toml::from_str("width = 700").unwrap();
        assert_eq!((parsed.width, parsed.language.as_str()), (700, ""));
    }

    #[test]
    fn auto_beautify_defaults_on_and_roundtrips() {
        let old: Settings = toml::from_str("width = 700").unwrap();
        assert!(old.auto_beautify);
        let off: Settings = toml::from_str("auto_beautify = false").unwrap();
        assert!(!off.auto_beautify);
    }

    #[test]
    fn theme_defaults_to_the_system_and_loads_when_set() {
        let old: Settings = toml::from_str("width = 700").unwrap();
        assert_eq!(old.theme, "");
        let dark: Settings = toml::from_str("theme = \"dark\"").unwrap();
        assert_eq!(dark.theme, "dark");
    }

    #[test]
    fn last_selection_roundtrips_and_defaults_to_none() {
        let old: Settings = toml::from_str("width = 700").unwrap();
        assert_eq!(
            (old.last_project.as_str(), old.last_folder, old.last_request),
            ("", None, None)
        );
        let s = Settings {
            last_project: "abc-1".into(),
            last_folder: Some(2),
            last_request: Some(0),
            ..Settings::default()
        };
        let back: Settings = toml::from_str(&toml::to_string_pretty(&s).unwrap()).unwrap();
        assert_eq!(
            (back.last_project.as_str(), back.last_folder, back.last_request),
            ("abc-1", Some(2), Some(0))
        );
    }

    #[test]
    fn invalid_file_is_an_error_not_defaults() {
        assert!(parse("width = \"wide\"").is_err());
    }

    #[test]
    fn window_size_is_clamped_both_ways() {
        let mut s: Settings = toml::from_str("width = 4000000000\nheight = 1").unwrap();
        s.normalize();
        assert_eq!((s.width, s.height), (MAX_SIZE, MIN_SIZE.1));
    }
}
