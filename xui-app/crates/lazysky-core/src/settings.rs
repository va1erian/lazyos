//! What the user can change, kept in `<app dir>/settings.toml`.
//!
//! A missing, unreadable or half-valid file never stops the app: every field
//! falls back to its default and out-of-range numbers are clamped. The
//! password is never part of this file; the handle is only when the user
//! asked to remember it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file's name inside the app directory.
pub const FILE: &str = "settings.toml";

/// The login service when none is configured.
pub const DEFAULT_SERVICE: &str = "https://bsky.social";

/// How the window looks: the host's theme, or one of the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

/// The tallest a lone picture in a post is drawn, in design pixels.
pub const MAX_IMAGE_HEIGHT: std::ops::RangeInclusive<u32> = 120..=2000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The login service; the account's PDS is read from its answer.
    pub service: String,
    pub theme: ThemeChoice,
    pub max_image_height: u32,
    /// Keep the handle (never the password) between runs.
    pub remember_handle: bool,
    pub handle: Option<String>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            service: DEFAULT_SERVICE.to_string(),
            theme: ThemeChoice::System,
            max_image_height: 420,
            remember_handle: false,
            handle: None,
        }
    }
}

impl Settings {
    /// Reads `dir/settings.toml`; anything wrong gives the defaults (and a
    /// bad number is clamped), never an error.
    pub fn load(dir: &Path) -> Settings {
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap_or_default();
        Settings::parse(&text)
    }

    pub fn parse(text: &str) -> Settings {
        let mut settings: Settings = toml::from_str(text).unwrap_or_default();
        settings.normalise();
        settings
    }

    /// Puts every field in its valid range.
    pub fn normalise(&mut self) {
        let service = self.service.trim().trim_end_matches('/').to_string();
        let web = service.starts_with("https://") || service.starts_with("http://");
        self.service = if web && service.len() > 8 {
            service
        } else {
            DEFAULT_SERVICE.to_string()
        };
        self.max_image_height = self
            .max_image_height
            .clamp(*MAX_IMAGE_HEIGHT.start(), *MAX_IMAGE_HEIGHT.end());
        if !self.remember_handle {
            self.handle = None;
        }
        self.handle = self
            .handle
            .take()
            .map(|h| h.trim().trim_start_matches('@').to_string())
            .filter(|h| !h.is_empty());
    }

    /// Writes `dir/settings.toml` (creating `dir`), through a temporary file
    /// so a crash never leaves half a file.
    pub fn save(&self, dir: &Path) -> Result<PathBuf, String> {
        let mut clean = self.clone();
        clean.normalise();
        let text = toml::to_string_pretty(&clean).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join(FILE);
        let temp = dir.join(format!("{FILE}.part"));
        std::fs::write(&temp, text).map_err(|e| format!("{}: {e}", temp.display()))?;
        std::fs::rename(&temp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lazysky-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_missing_file_is_the_defaults() {
        assert_eq!(Settings::load(&scratch("missing")), Settings::default());
    }

    #[test]
    fn settings_round_trip_through_the_file() {
        let dir = scratch("roundtrip");
        let settings = Settings {
            service: "https://pds.example.test".into(),
            theme: ThemeChoice::Dark,
            max_image_height: 300,
            remember_handle: true,
            handle: Some("alice.test".into()),
        };
        settings.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir), settings);
        assert!(!dir.join("settings.toml.part").exists());
    }

    #[test]
    fn the_handle_is_not_kept_unless_asked() {
        let dir = scratch("handle");
        let settings = Settings {
            handle: Some("alice.test".into()),
            remember_handle: false,
            ..Settings::default()
        };
        settings.save(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join("settings.toml")).unwrap();
        assert!(!text.contains("alice"), "{text}");
        assert_eq!(Settings::load(&dir).handle, None);
    }

    #[test]
    fn nonsense_is_repaired_not_fatal() {
        let s =
            Settings::parse("service = \"ftp://x\"\nmax_image_height = 99999\ntheme = \"purple\"");
        // An invalid theme fails the whole file: defaults, not a crash.
        assert_eq!(s, Settings::default());
        let s = Settings::parse("service = \"ftp://x\"\nmax_image_height = 99999");
        assert_eq!(s.service, DEFAULT_SERVICE);
        assert_eq!(s.max_image_height, 2000);
        let s = Settings::parse("not toml at all [[[");
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn the_service_is_tidied() {
        let s = Settings::parse("service = \" https://pds.test/ \"");
        assert_eq!(s.service, "https://pds.test");
        let s = Settings::parse("remember_handle = true\nhandle = \"@bob.test\"");
        assert_eq!(s.handle.as_deref(), Some("bob.test"));
    }
}
