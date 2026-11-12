//! Where the NES Player keeps its files, under its `fhs::app_data_dir`.
//!
//! Nothing here trusts a ROM's filename: save files are named from a
//! sanitised stem, so a ROM called `../../etc/passwd` cannot make the app
//! write outside its own directory (docs/security-model.md).

use std::path::{Path, PathBuf};

/// Longest file stem a save file is named from.
const MAX_STEM: usize = 64;

/// A ROM's file stem as a save-file name: ASCII letters, digits, `-`, `_`
/// and `.` are kept, anything else becomes `_`, the result is capped, and
/// leading and trailing dots are trimmed (so a name cannot hide a file). A
/// stem that keeps nothing is `rom`.
pub fn safe_stem(name: &str) -> String {
    let mut out = String::new();
    for byte in name.bytes().take(MAX_STEM) {
        let keep = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.');
        out.push(if keep { char::from(byte) } else { '_' });
    }
    let trimmed = out.trim_matches('.').to_string();
    if trimmed.is_empty() || trimmed.chars().all(|c| c == '_') {
        return "rom".to_string();
    }
    trimmed
}

/// The stem a ROM's files are named from: its file name without the extension.
pub fn rom_stem(rom: &Path) -> String {
    let name = rom
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    safe_stem(&name)
}

/// The battery save for `rom`.
pub fn sram_path(dir: &Path, rom: &Path) -> PathBuf {
    dir.join("saves").join(format!("{}.sram", rom_stem(rom)))
}

/// Save-state slot `slot` (1-based) for `rom`.
pub fn state_path(dir: &Path, rom: &Path, slot: usize) -> PathBuf {
    dir.join("states")
        .join(rom_stem(rom))
        .join(format!("slot-{slot}.state"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stem_keeps_only_safe_characters() {
        assert_eq!(safe_stem("Super Mario Bros. 3"), "Super_Mario_Bros._3");
        assert_eq!(safe_stem("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(safe_stem("game.nes"), "game.nes");
        assert_eq!(safe_stem(""), "rom");
        assert_eq!(safe_stem("///"), "rom");
        assert_eq!(safe_stem("..."), "rom");
        assert_eq!(safe_stem(&"x".repeat(200)).len(), MAX_STEM);
    }

    #[test]
    fn a_save_path_stays_under_the_app_directory() {
        let dir = Path::new("/home/user/.apps/os.lazy.nes");
        // The stem is the path's own last component, so a directory in the
        // path can never reach a save file.
        let path = sram_path(dir, Path::new("/roms/../etc/passwd"));
        assert!(path.starts_with(dir), "{path:?}");
        assert_eq!(path.file_name().unwrap().to_string_lossy(), "passwd.sram");
        // A name that would hide the file, or keep nothing, becomes `rom`.
        let hidden = sram_path(dir, Path::new("/roms/....nes"));
        assert_eq!(hidden.file_name().unwrap().to_string_lossy(), "rom.sram");
        let state = state_path(dir, Path::new("/roms/zelda.nes"), 2);
        assert!(state.starts_with(dir), "{state:?}");
        assert!(state.ends_with("states/zelda/slot-2.state"), "{state:?}");
    }
}
