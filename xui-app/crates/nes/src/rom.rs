//! What a cartridge file is: its iNES/NES 2.0 header, the description the
//! window shows, and the checksum a `--headless` run prints.
//!
//! Only what the app displays is read from the header — the mapper number,
//! the PRG/CHR sizes and the battery flag. Everything else is the emulator
//! core's business, and the file itself is untrusted input: the header check
//! refuses anything that is not a cartridge before the core sees it.

use tetanes_core::prelude::NesRegion;

/// What the iNES header says about a cartridge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub mapper: u16,
    pub prg_kb: usize,
    pub chr_kb: usize,
    pub battery: bool,
}

/// The iNES or NES 2.0 header of `bytes`, or `None` when it is not one.
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
    pub path: std::path::PathBuf,
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

    #[test]
    fn a_summary_names_the_cartridge() {
        let info = RomInfo {
            path: std::path::PathBuf::from("/roms/sample.nes"),
            name: "sample".to_string(),
            mapper: 0,
            prg_kb: 32,
            chr_kb: 8,
            battery: true,
            region: NesRegion::Ntsc,
        };
        assert_eq!(info.summary(), "sample (mapper 0, 32K PRG, 8K CHR, ntsc)");
    }
}
