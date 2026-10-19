//! What the user can change, kept in `<app dir>/settings.toml`
//! (docs/ai-agent-plan.md §9.3). No secret is ever part of it: the API key is
//! held in memory only (§5.3).
//!
//! A missing, unreadable or half-valid file never stops the app: every field
//! falls back to its default and out-of-range numbers are clamped.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model::DEFAULT_MODEL;
use crate::provider::Effort;

/// The file's name inside the app directory.
pub const FILE: &str = "settings.toml";

/// How the window looks: the host's theme, or one of the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The model id sent with every request.
    pub model: String,
    /// `none`, `low`, `medium` or `high`; empty leaves it to the model.
    pub effort: String,
    /// 0.0 to 2.0; `None` leaves it to the model.
    pub temperature: Option<f32>,
    /// `None` leaves it to the model.
    pub max_tokens: Option<u32>,
    /// The user has read what is sent where (the first-run consent screen).
    pub consented: bool,
    pub theme: ThemeChoice,
    /// A free-text addition to the system prompt.
    pub system: String,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            model: DEFAULT_MODEL.to_string(),
            effort: String::new(),
            temperature: None,
            max_tokens: None,
            consented: false,
            theme: ThemeChoice::System,
            system: String::new(),
        }
    }
}

/// The longest model id and custom system prompt kept.
const MAX_MODEL: usize = 200;
const MAX_SYSTEM: usize = 4000;

impl Settings {
    /// Reads `dir/settings.toml`; anything wrong gives the defaults.
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
        let model: String = self
            .model
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_MODEL)
            .collect();
        self.model = if model.is_empty() {
            DEFAULT_MODEL.to_string()
        } else {
            model
        };
        if !self.effort.is_empty() && Effort::parse(&self.effort).is_none() {
            self.effort.clear();
        }
        self.temperature = self
            .temperature
            .filter(|t| t.is_finite())
            .map(|t| t.clamp(0.0, 2.0));
        self.max_tokens = self.max_tokens.map(|n| n.clamp(16, 1_000_000));
        self.system = self.system.chars().take(MAX_SYSTEM).collect();
    }

    /// Writes the file, through a rename so a crash leaves the old one.
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let text = toml::to_string_pretty(self).map_err(io::Error::other)?;
        let target = dir.join(FILE);
        let temp = dir.join(format!("{FILE}.tmp"));
        std::fs::write(&temp, text)?;
        std::fs::rename(&temp, target)
    }

    /// The effort as the provider takes it.
    pub fn reasoning_effort(&self) -> Option<Effort> {
        Effort::parse(&self.effort)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_gives_the_defaults_and_numbers_are_clamped() {
        assert_eq!(Settings::parse("not = [valid"), Settings::default());
        let s = Settings::parse(
            "model = \"  \"\neffort = \"loud\"\ntemperature = 9.0\nmax_tokens = 1\n",
        );
        assert_eq!(s.model, DEFAULT_MODEL);
        assert_eq!(s.effort, "");
        assert_eq!(s.temperature, Some(2.0));
        assert_eq!(s.max_tokens, Some(16));
        let s = Settings::parse("model = \"a\\nb\"");
        assert_eq!(s.model, "ab");
    }

    #[test]
    fn settings_round_trip_through_the_file() {
        let dir = std::env::temp_dir().join(format!("lazyagent-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = Settings {
            consented: true,
            effort: "low".into(),
            theme: ThemeChoice::Dark,
            ..Settings::default()
        };
        s.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir), s);
        assert_eq!(s.reasoning_effort(), Some(Effort::Low));
        assert!(!std::fs::read_to_string(dir.join(FILE))
            .unwrap()
            .contains("key"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
