//! The options the app remembers between runs: `$dir/config.txt`.
//!
//! A tiny `key=value` file rather than `confd`: these are one app's display
//! and sound preferences, and a malformed line must never stop a game from
//! starting — every value is validated, and anything unrecognised is ignored.

use std::path::{Path, PathBuf};

use crate::session::MAX_SLOTS;
use crate::video::{Aspect, Scale};

/// The file's name in the app's directory.
const FILE: &str = "config.txt";

/// What the app remembers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// How the picture is scaled.
    pub scale: Scale,
    /// The pixel shape.
    pub aspect: Aspect,
    /// Whether the composite (NTSC) filter is on.
    pub ntsc: bool,
    /// Whether the FPS counter is drawn.
    pub show_fps: bool,
    /// The save-state slot the commands act on.
    pub slot: usize,
    /// Whether the mixer stream starts muted.
    pub muted: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            scale: Scale::Auto,
            aspect: Aspect::Pixel8x7,
            ntsc: false,
            show_fps: false,
            slot: 1,
            muted: false,
        }
    }
}

/// The file's path in `dir`.
pub fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

impl Settings {
    /// Reads the settings from `dir`; a missing or unreadable file is the
    /// defaults, never an error.
    pub fn load(dir: &Path) -> Settings {
        let Ok(text) = std::fs::read_to_string(path(dir)) else {
            return Settings::default();
        };
        let mut settings = Settings::default();
        for line in text.lines().take(64) {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "scale" => {
                    settings.scale = match value {
                        "1" => Scale::Fixed(1),
                        "2" => Scale::Fixed(2),
                        "3" => Scale::Fixed(3),
                        _ => Scale::Auto,
                    }
                }
                "aspect" => settings.aspect = if value == "square" { Aspect::Square } else { Aspect::Pixel8x7 },
                "ntsc" => settings.ntsc = value == "1",
                "fps" => settings.show_fps = value == "1",
                "slot" => {
                    if let Ok(slot) = value.parse::<usize>() {
                        settings.slot = slot.clamp(1, MAX_SLOTS);
                    }
                }
                "mute" => settings.muted = value == "1",
                _ => {}
            }
        }
        settings
    }

    /// Writes the settings to `dir`, creating it when needed.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let scale = match self.scale {
            Scale::Auto => "auto",
            Scale::Fixed(1) => "1",
            Scale::Fixed(2) => "2",
            Scale::Fixed(_) => "3",
        };
        let aspect = match self.aspect {
            Aspect::Pixel8x7 => "pixel",
            Aspect::Square => "square",
        };
        let text = format!(
            "# NES Player settings (docs/nes-player.md)\n\
             scale={scale}\naspect={aspect}\nntsc={}\nfps={}\nslot={}\nmute={}\n",
            u8::from(self.ntsc),
            u8::from(self.show_fps),
            self.slot,
            u8::from(self.muted),
        );
        std::fs::create_dir_all(dir)?;
        std::fs::write(path(dir), text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        let dir = std::env::temp_dir().join("xui-nes-settings-test");
        let _ = std::fs::remove_dir_all(&dir);
        let settings = Settings {
            scale: Scale::Fixed(3),
            aspect: Aspect::Square,
            ntsc: true,
            show_fps: true,
            slot: 4,
            muted: true,
        };
        settings.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir), settings);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_or_broken_file_is_the_defaults() {
        let dir = std::env::temp_dir().join("xui-nes-settings-broken");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(Settings::load(&dir), Settings::default());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(path(&dir), "nonsense\nscale=99\nslot=99\n=1\nfps=maybe\n").unwrap();
        let loaded = Settings::load(&dir);
        assert_eq!(loaded.scale, Scale::Auto);
        assert_eq!(loaded.slot, MAX_SLOTS);
        assert!(!loaded.show_fps);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
