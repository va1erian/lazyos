//! The user agents the View menu offers: the well-known ones of the big browsers, for sites that
//! turn the engine's own away, and the engine's own (the first entry, an empty agent).
//!
//! The choice is a file in the profile folder ([`FILE`], the preset's id) read before the window
//! opens, so a new window starts with it. An agent is no authority, so unlike the ad blocker's
//! switches it needs no place out of the engine's reach; a name this table does not know is the
//! engine's own.

use std::io;
use std::path::Path;

/// The file in the profile folder that names the chosen preset.
pub const FILE: &str = "user-agent";

/// One entry of the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preset {
    /// What the file holds.
    pub id: &'static str,
    /// The menu text.
    pub name: &'static str,
    /// The agent; empty for the engine's own.
    pub agent: &'static str,
}

/// The entries, the engine's own first. Browsers freeze the platform part of their agent, so these
/// stay true to what the real ones send; the versions are those of a recent stable release.
pub const PRESETS: [Preset; 8] = [
    Preset {
        id: "default",
        name: "LazyWeb (default)",
        agent: "",
    },
    Preset {
        id: "chrome-windows",
        name: "Chrome on Windows",
        agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36",
    },
    Preset {
        id: "chrome-mac",
        name: "Chrome on macOS",
        agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36",
    },
    Preset {
        id: "edge-windows",
        name: "Edge on Windows",
        agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36 Edg/140.0.0.0",
    },
    Preset {
        id: "firefox-windows",
        name: "Firefox on Windows",
        agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:143.0) Gecko/20100101 Firefox/143.0",
    },
    Preset {
        id: "firefox-linux",
        name: "Firefox on Linux",
        agent: "Mozilla/5.0 (X11; Linux x86_64; rv:143.0) Gecko/20100101 Firefox/143.0",
    },
    Preset {
        id: "safari-mac",
        name: "Safari on macOS",
        agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.6 Safari/605.1.15",
    },
    Preset {
        id: "safari-iphone",
        name: "Safari on iPhone",
        agent: "Mozilla/5.0 (iPhone; CPU iPhone OS 18_6 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.6 Mobile/15E148 Safari/604.1",
    },
];

/// The preset named `id`; the engine's own for a name nobody knows.
pub fn by_id(id: &str) -> usize {
    PRESETS
        .iter()
        .position(|preset| preset.id == id.trim())
        .unwrap_or(0)
}

/// The preset `profile` records (the engine's own without a file).
pub fn load(profile: &Path) -> usize {
    std::fs::read_to_string(profile.join(FILE)).map_or(0, |text| by_id(&text))
}

/// Records preset `index` in `profile`; the engine's own removes the file.
pub fn save(profile: &Path, index: usize) -> io::Result<()> {
    let file = profile.join(FILE);
    match PRESETS.get(index).filter(|_| index != 0) {
        Some(preset) => std::fs::write(file, preset.id),
        None => match std::fs::remove_file(file) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lazyweb-useragent-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn every_agent_is_one_the_engine_takes() {
        // webd: printable ASCII, at most 256 bytes (`idl/webd.midl`, SetUserAgent).
        for preset in PRESETS {
            assert!(preset.agent.len() <= 256, "{}", preset.id);
            assert!(
                preset.agent.bytes().all(|b| (0x20..0x7f).contains(&b)),
                "{}",
                preset.id
            );
        }
    }

    #[test]
    fn ids_and_names_are_unique_and_the_first_is_the_engines_own() {
        assert_eq!(PRESETS[0].agent, "");
        for (n, a) in PRESETS.iter().enumerate() {
            for b in &PRESETS[n + 1..] {
                assert!(a.id != b.id && a.name != b.name);
                assert!(a.agent != b.agent);
            }
            assert!(n == 0 || a.agent.starts_with("Mozilla/5.0 ("), "{}", a.id);
        }
    }

    #[test]
    fn an_unknown_name_is_the_engines_own() {
        assert_eq!(by_id("firefox-linux"), 5);
        assert_eq!(by_id(" safari-mac\n"), 6);
        assert_eq!(by_id("netscape"), 0);
        assert_eq!(by_id(""), 0);
    }

    #[test]
    fn the_choice_is_kept_in_the_profile() {
        let dir = scratch("keep");
        assert_eq!(load(&dir), 0, "no file");
        save(&dir, 4).unwrap();
        assert_eq!(load(&dir), 4);
        save(&dir, 0).unwrap();
        assert_eq!(load(&dir), 0);
        assert!(
            !dir.join(FILE).exists(),
            "the engine's own removes the file"
        );
        save(&dir, 0).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
