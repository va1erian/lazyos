//! `--headless`: run a cartridge with no window or mixer, for a harness.
//!
//! `nes.elf --headless <rom> [--frames N]` is the deterministic check, like
//! Doom's `-frames` and Quake's `-headless`: the console runs a fixed number
//! of display frames and the final frame's CRC-32 is the verdict. The logic
//! lives here (not in the binary) so it is host-testable; the binary only
//! parses the arguments, prints the marker and writes the result file.

use std::path::Path;

use crate::rom::{self, RomInfo};
use crate::session::{self, Session};

/// Frames a headless run clocks when the caller does not say.
pub const DEFAULT_FRAMES: u64 = 600;

/// What a headless run saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// Display frames clocked.
    pub frames: u64,
    /// The CRC-32 of the final frame, as RGBA pixels.
    pub crc: u32,
    /// The cartridge, as the window would describe it.
    pub info: RomInfo,
}

/// Runs `rom` for `frames` display frames with no window and no mixer, and
/// returns the final frame's checksum. `dir` is the app's directory: the
/// cartridge's battery file is read and written there like any other run.
pub fn run(dir: &Path, rom: &Path, frames: u64) -> session::Result<Report> {
    let mut session = Session::new(dir.to_path_buf(), session::SAMPLE_RATE);
    let info = session.load_rom(rom)?;
    for _ in 0..frames {
        session.clock()?;
    }
    let crc = rom::crc32(session.frame());
    Ok(Report { frames, crc, info })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sample cartridge the image ships, as the crate's tests have it.
    const SAMPLE: &[u8] = include_bytes!("../tests/data/sample.nes");

    fn dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("xui-nes-headless-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a test directory");
        dir
    }

    fn rom(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("sample.nes");
        std::fs::write(&path, SAMPLE).expect("the sample ROM");
        path
    }

    #[test]
    fn a_headless_run_is_deterministic() {
        let dir = dir("twice");
        let rom = rom(&dir);
        let first = run(&dir, &rom, 30).expect("the first run");
        let second = run(&dir, &rom, 30).expect("the second run");
        assert_eq!(first, second, "the same ROM and frame count agree");
        assert_eq!(first.frames, 30);
        assert_eq!(first.info.mapper, 0);
        assert_ne!(first.crc, 0, "a real picture has a checksum");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_rom_fails() {
        let dir = dir("missing");
        assert!(run(&dir, &dir.join("gone.nes"), 1).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
