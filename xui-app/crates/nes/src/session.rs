//! The emulator session: one NES, its ROM, its save files, its pacing.
//!
//! [`tetanes_core`] owns the console (CPU, PPU, APU, mappers, save states);
//! this wraps it for the app: loading a ROM with a size cap and a header
//! check, the battery file and the save-state slots under the app's own
//! directory, and the small state a window needs (paused, fast-forward, the
//! selected slot). The deck is configured with no filesystem of its own
//! (`Config::with_sram_dir(None)`), so every path this app writes is named
//! here, from a sanitised ROM name.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use tetanes_core::input::JoypadBtn;
use tetanes_core::prelude::{Clocked, Config, ControlDeck, NesRegion, Player, RamState, ResetKind};
use tetanes_core::video::VideoFilter;

use crate::paths;

/// Largest ROM the app reads: the biggest licensed cartridges are 6 MiB.
pub const MAX_ROM_BYTES: u64 = 16 * 1024 * 1024;
/// The rate the APU is asked for, and the stream opened at.
pub const SAMPLE_RATE: f32 = 48_000.0;

/// Why an operation failed.
#[derive(Debug)]
pub enum Error {
    /// A file could not be read or written.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The file is larger than [`MAX_ROM_BYTES`].
    TooLarge(u64),
    /// The file is not an iNES/NES 2.0 cartridge.
    BadRom(String),
    /// The console refused the ROM or an emulated frame.
    Emulation(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Error::TooLarge(bytes) => write!(f, "the file is {bytes} bytes, too large for a ROM"),
            Error::BadRom(why) => write!(f, "{why}"),
            Error::Emulation(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// What the iNES header says about a cartridge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub mapper: u16,
    pub prg_kb: usize,
    pub chr_kb: usize,
    pub battery: bool,
}

/// The iNES or NES 2.0 header of `bytes`, or `None` when it is not one./// Only what the app displays is read: the mapper number, the PRG/CHR sizes
/// and the battery flag. Everything else is the emulator core's business.
pub fn parse_header(bytes: &[u8]) -> Option<Header> {
    if bytes.len() < 16 || &bytes[0..4] != b"NES\x1a" {
        return None;
    }
    let (prg_units, chr_units) = (bytes[4], bytes[5]);
    if prg_units == 0 {
        return None;
    }
    let mut mapper = u16::from(bytes[6] >> 4) | u16::from(bytes[7] & 0xF0);
    if bytes[7] & 0x0C == 0x08 {
        // NES 2.0: four more mapper bits in the next byte.
        mapper |= u16::from(bytes[8] & 0x0F) << 8;
    }
    Some(Header {
        mapper,
        prg_kb: usize::from(prg_units) * 16,
        chr_kb: usize::from(chr_units) * 8,
        battery: bytes[6] & 0x02 != 0,
    })
}

/// The CRC-32 of `bytes`: the deterministic frame checksum a harness reads
/// from a `--headless` run.
pub fn crc32(bytes: &[u8]) -> u32 {
    tetanes_core::fs::compute_crc32(bytes)
}

/// The region name the evidence lines and the status bar use.
pub fn region_name(region: NesRegion) -> &'static str {
    match region {
        NesRegion::Auto => "auto",
        NesRegion::Ntsc => "ntsc",
        NesRegion::Pal => "pal",
        NesRegion::Dendy => "dendy",
    }
}

/// A loaded ROM, as the window and the evidence describe it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RomInfo {
    pub path: PathBuf,
    pub name: String,
    /// The mapper the header names (the core may prefer its game database).
    pub mapper: u16,
    pub prg_kb: usize,
    pub chr_kb: usize,
    pub battery: bool,
    pub region: NesRegion,
}

impl RomInfo {
    /// The line the app reports and the status bar shows.
    pub fn summary(&self) -> String {
        format!(
            "{} (mapper {}, {}K PRG, {}K CHR, {})",
            self.name,
            self.mapper,
            self.prg_kb,
            self.chr_kb,
            region_name(self.region)
        )
    }
}

/// One running NES.
pub struct Session {
    deck: ControlDeck,
    /// The app's directory (`$HOME/.apps/os.lazy.nes`).
    dir: PathBuf,
    /// The rate the APU runs at (the mixer's grant).
    rate: f32,
    info: Option<RomInfo>,
    paused: bool,
    fast_forward: bool,
    frames: u64,
    slot: usize,
    /// The region the user chose, or `None` to follow the cartridge.
    region: Option<NesRegion>,
    /// Whether the composite (NTSC) filter is on.
    ntsc: bool,
    /// Something the user should know but that is not fatal (a corrupt
    /// battery file, a save that could not be written).
    warning: Option<String>,
}

impl Session {
    /// A console with no cartridge. `sample_rate` is what the audio stream
    /// was granted (the APU is set to it, so sound never plays at the wrong
    /// pitch).
    pub fn new(dir: PathBuf, sample_rate: f32) -> Session {
        let config = Config::default()
            .with_sram_dir(None)
            .with_ram_state(RamState::AllZeros)
            // Straight palette pixels by default: crisp, and a fraction of the
            // cost of the composite simulation (which the Video menu offers).
            .with_filter(VideoFilter::Pixellate);
        let mut deck = ControlDeck::with_config(config);
        deck.set_sample_rate(sample_rate);
        Session {
            deck,
            dir,
            rate: sample_rate,
            info: None,
            paused: false,
            fast_forward: false,
            frames: 0,
            slot: 1,
            region: None,
            ntsc: false,
            warning: None,
        }
    }

    /// Loads `path` as a ROM, restoring its battery file. A previous ROM's
    /// battery is written first.
    pub fn load_rom(&mut self, path: &Path) -> Result<RomInfo> {
        let metadata = fs::metadata(path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.len() > MAX_ROM_BYTES {
            return Err(Error::TooLarge(metadata.len()));
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        File::open(path)
            .and_then(|file| file.take(MAX_ROM_BYTES + 1).read_to_end(&mut bytes))
            .map_err(|source| Error::Io {
                path: path.to_path_buf(),
                source,
            })?;
        if bytes.len() as u64 > MAX_ROM_BYTES {
            // The file grew since `metadata`: read no further than the cap.
            return Err(Error::TooLarge(bytes.len() as u64));
        }
        let header = parse_header(&bytes)
            .ok_or_else(|| Error::BadRom("not an iNES or NES 2.0 cartridge".to_string()))?;
        self.unload();
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "rom".to_string());
        let loaded = self
            .deck
            .load_rom(name.clone(), &mut &bytes[..])
            .map_err(|error| Error::Emulation(error.to_string()))?;
        let info = RomInfo {
            path: path.to_path_buf(),
            name,
            mapper: header.mapper,
            prg_kb: header.prg_kb,
            chr_kb: header.chr_kb,
            battery: loaded.battery_backed,
            region: loaded.region,
        };
        if info.battery {
            let save = paths::sram_path(&self.dir, path);
            if save.exists() {
                if let Err(error) = self.deck.load_sram_path(&save) {
                    self.warning = Some(format!("battery save not read: {error}"));
                }
            }
        }
        self.deck.set_region(self.region.unwrap_or(info.region));
        self.info = Some(info.clone());
        self.frames = 0;
        self.paused = false;
        Ok(info)
    }

    /// Unloads the cartridge, writing its battery file first.
    pub fn unload(&mut self) {
        if let Some(warning) = self.save_battery().err() {
            self.warning = Some(warning.to_string());
        }
        if self.info.take().is_some() {
            let _ = self.deck.unload_rom();
        }
    }

    /// Clocks one display frame.
    pub fn clock(&mut self) -> Result<()> {
        loop {
            match self.deck.clock_frame() {
                Ok(Clocked::Continue) => {}
                Ok(_) => break,
                Err(error) => return Err(Error::Emulation(error.to_string())),
            }
        }
        self.frames += 1;
        Ok(())
    }

    /// The last clocked frame as RGBA pixels (256x240).
    pub fn frame(&mut self) -> &[u8] {
        self.deck.frame_buffer()
    }

    /// The last clocked frame's audio: mono `f32` at the session's rate.
    pub fn samples(&self) -> &[f32] {
        self.deck.audio_samples()
    }

    /// How many samples one display frame produces at `rate`.
    pub fn frame_samples(rate: f32) -> usize {
        (f64::from(rate.max(1.0)) * (crate::pacer::FRAME_NANOS as f64 / 1e9)) as usize
    }

    /// How many samples one display frame produces at this session's rate.
    pub fn samples_per_frame(&self) -> usize {
        Session::frame_samples(self.rate)
    }

    /// The rate the APU and the mixer stream run at.
    pub fn sample_rate(&self) -> f32 {
        self.rate
    }

    /// One joypad button.
    pub fn press(&mut self, button: JoypadBtn, down: bool) {
        self.deck.joypad_mut(Player::One).set_button(button, down);
    }

    pub fn info(&self) -> Option<&RomInfo> {
        self.info.as_ref()
    }

    pub fn is_loaded(&self) -> bool {
        self.info.is_some()
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    pub fn fast_forward(&self) -> bool {
        self.fast_forward
    }

    pub fn set_fast_forward(&mut self, on: bool) {
        self.fast_forward = on;
    }

    /// The emulation speed the pacer runs at.
    pub fn speed(&self) -> f64 {
        if self.fast_forward {
            2.0
        } else {
            1.0
        }
    }

    /// Display frames clocked since the ROM was loaded.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// `true` for a power cycle (zeroed RAM), `false` for the console's
    /// reset button.
    pub fn reset(&mut self, hard: bool) {
        let kind = if hard {
            ResetKind::Hard
        } else {
            ResetKind::Soft
        };
        self.deck.reset(kind);
        self.frames = 0;
        self.paused = false;
    }

    /// Unloads and reloads the cartridge: the console's power switch.
    pub fn power_cycle(&mut self) -> Result<()> {
        let Some(path) = self.info.as_ref().map(|info| info.path.clone()) else {
            return Ok(());
        };
        self.load_rom(&path).map(|_| ())
    }

    /// Follows the cartridge's region, or `region` when one is chosen.
    pub fn set_region(&mut self, region: Option<NesRegion>) {
        self.region = region;
        if let Some(info) = self.info.clone() {
            self.deck.set_region(region.unwrap_or(info.region));
        }
    }

    pub fn region(&self) -> Option<NesRegion> {
        self.region
    }

    /// Whether the composite (NTSC) filter is on.
    pub fn ntsc(&self) -> bool {
        self.ntsc
    }

    /// Switches between straight palette pixels and the composite simulation.
    pub fn set_ntsc(&mut self, on: bool) {
        self.ntsc = on;
        let filter = if on {
            VideoFilter::Ntsc
        } else {
            VideoFilter::Pixellate
        };
        self.deck.set_filter(filter);
    }

    /// The save-state slot commands act on, 1-based.
    pub fn slot(&self) -> usize {
        self.slot
    }

    pub fn set_slot(&mut self, slot: usize) {
        self.slot = slot.clamp(1, MAX_SLOTS);
    }

    /// The file a slot saves to.
    pub fn state_path(&self, slot: usize) -> Option<PathBuf> {
        self.info
            .as_ref()
            .map(|info| paths::state_path(&self.dir, &info.path, slot))
    }

    /// Whether the slot has a state to load.
    pub fn has_state(&self, slot: usize) -> bool {
        self.state_path(slot).is_some_and(|path| path.exists())
    }

    /// Saves the whole console to the slot's file.
    pub fn save_state(&mut self, slot: usize) -> Result<PathBuf> {
        let path = self
            .state_path(slot)
            .ok_or_else(|| Error::BadRom("no ROM is loaded".to_string()))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| Error::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        self.deck
            .save_state_path(&path)
            .map_err(|error| Error::Emulation(error.to_string()))?;
        Ok(path)
    }

    /// Restores the console from the slot's file.
    pub fn load_state(&mut self, slot: usize) -> Result<PathBuf> {
        let path = self
            .state_path(slot)
            .ok_or_else(|| Error::BadRom("no ROM is loaded".to_string()))?;
        self.deck
            .load_state_path(&path)
            .map_err(|error| Error::Emulation(error.to_string()))?;
        self.paused = false;
        Ok(path)
    }

    /// Writes the cartridge's battery file, when it has one.
    pub fn save_battery(&mut self) -> Result<Option<PathBuf>> {
        let Some(info) = self.info.clone() else {
            return Ok(None);
        };
        if !info.battery {
            return Ok(None);
        }
        let path = paths::sram_path(&self.dir, &info.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| Error::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        self.deck
            .save_sram_path(&path)
            .map_err(|error| Error::Emulation(error.to_string()))?;
        Ok(Some(path))
    }

    /// The console's work RAM, for tests and diagnostics.
    pub fn wram(&self) -> &[u8; 2048] {
        self.deck.wram()
    }

    /// The cartridge's battery RAM.
    pub fn sram(&mut self) -> &[u8] {
        self.deck.sram()
    }

    /// The last warning, taken.
    pub fn take_warning(&mut self) -> Option<String> {
        self.warning.take()
    }
}

/// How many save-state slots a ROM has.
pub const MAX_SLOTS: usize = 4;

impl Drop for Session {
    fn drop(&mut self) {
        // The app's exit path saves too; this is the backstop for a panic.
        let _ = self.save_battery();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_is_read_and_a_non_rom_is_not() {
        let mut bytes = vec![0u8; 16];
        bytes[..4].copy_from_slice(b"NES\x1a");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[6] = 0x02;
        assert_eq!(
            parse_header(&bytes),
            Some(Header {
                mapper: 0,
                prg_kb: 32,
                chr_kb: 8,
                battery: true
            })
        );
        bytes[6] = 0x40;
        bytes[7] = 0x10;
        assert_eq!(parse_header(&bytes).unwrap().mapper, 0x14);
        bytes[7] = 0x08;
        bytes[8] = 0x03;
        assert_eq!(parse_header(&bytes).unwrap().mapper, 0x304);
        assert_eq!(parse_header(b"not a rom"), None);
        bytes[4] = 0;
        assert_eq!(parse_header(&bytes), None);
    }

    #[test]
    fn regions_have_names() {
        assert_eq!(region_name(NesRegion::Ntsc), "ntsc");
        assert_eq!(region_name(NesRegion::Pal), "pal");
        assert_eq!(region_name(NesRegion::Auto), "auto");
    }
}
