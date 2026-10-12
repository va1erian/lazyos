//! The CEA-861 EDID extension (tag 0x02): data blocks and detailed timings.
//!
//! Layout (CTA-861-G section 7): byte 0 tag, byte 1 revision, byte 2 `d`,
//! the offset where detailed timing descriptors start (and where the data
//! blocks end), byte 3 capability flags, data blocks from byte 4 to `d`, DTDs
//! from `d` until a zero pixel clock, byte 127 checksum.

use alloc::vec::Vec;

use crate::edid::Timing;

/// Audio format codes of a short audio descriptor (CTA-861-G table 37).
pub mod format {
    pub const LPCM: u8 = 1;
    pub const AC3: u8 = 2;
    pub const MPEG1: u8 = 3;
    pub const MP3: u8 = 4;
    pub const MPEG2: u8 = 5;
    pub const AAC_LC: u8 = 6;
    pub const DTS: u8 = 7;
    pub const ATRAC: u8 = 8;
    pub const DD_PLUS: u8 = 10;
    pub const DTS_HD: u8 = 11;
    pub const MAT: u8 = 12;
}

/// The sample rates a SAD's rate byte names, bit 0 first.
pub const SAD_RATES_HZ: [u32; 7] = [32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

/// A short audio descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sad {
    /// Audio format code (see [`format`]).
    pub format: u8,
    /// Maximum channel count (1 to 8).
    pub channels: u8,
    /// Bit `i` set: [`SAD_RATES_HZ`]`[i]` supported.
    pub rates: u8,
    /// Third byte: for LPCM bits 0 to 2 are 16/20/24-bit support, for the
    /// compressed formats up to 8 the maximum bit rate in units of 8 kb/s.
    pub detail: u8,
}

impl Sad {
    pub fn from_bytes(b: [u8; 3]) -> Sad {
        Sad {
            format: (b[0] >> 3) & 0x0F,
            channels: (b[0] & 7) + 1,
            rates: b[1] & 0x7F,
            detail: b[2],
        }
    }

    pub fn to_bytes(self) -> [u8; 3] {
        [
            (self.format & 0x0F) << 3 | (self.channels.clamp(1, 8) - 1),
            self.rates & 0x7F,
            self.detail,
        ]
    }

    pub fn is_lpcm(&self) -> bool {
        self.format == format::LPCM
    }

    pub fn supports_rate(&self, hz: u32) -> bool {
        SAD_RATES_HZ
            .iter()
            .position(|r| *r == hz)
            .is_some_and(|i| self.rates & (1 << i) != 0)
    }

    /// LPCM sample sizes as a bit mask: bit 0 16-bit, bit 1 20, bit 2 24.
    pub fn lpcm_depths(&self) -> u8 {
        if self.is_lpcm() {
            self.detail & 7
        } else {
            0
        }
    }
}

/// A short video descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Svd {
    /// Video identification code (CTA-861 table 1).
    pub vic: u8,
    /// Native format (only meaningful for VIC 1 to 64).
    pub native: bool,
}

/// Parameters of a few common VICs: `(width, height, refresh Hz, interlaced)`.
/// Not the whole CTA table: the formats a HDMI 1.4 home monitor lists first.
pub fn vic_format(vic: u8) -> Option<(u16, u16, u8, bool)> {
    Some(match vic {
        1 => (640, 480, 60, false),
        2 | 3 => (720, 480, 60, false),
        4 => (1280, 720, 60, false),
        5 => (1920, 1080, 60, true),
        16 => (1920, 1080, 60, false),
        17 | 18 => (720, 576, 50, false),
        19 => (1280, 720, 50, false),
        20 => (1920, 1080, 50, true),
        31 => (1920, 1080, 50, false),
        32 => (1920, 1080, 24, false),
        33 => (1920, 1080, 25, false),
        34 => (1920, 1080, 30, false),
        93 => (3840, 2160, 24, false),
        94 => (3840, 2160, 25, false),
        95 => (3840, 2160, 30, false),
        97 => (3840, 2160, 60, false),
        _ => return None,
    })
}

/// The HDMI 1.x vendor-specific data block (OUI 00-0C-03).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HdmiVsdb {
    /// CEC physical address, nibbles A.B.C.D.
    pub phys_addr: u16,
    pub supports_ai: bool,
    pub dc_30bit: bool,
    pub dc_36bit: bool,
    pub dc_48bit: bool,
    pub dc_y444: bool,
    pub dvi_dual: bool,
    /// Maximum TMDS clock in MHz (the block counts 5 MHz units).
    pub max_tmds_mhz: Option<u16>,
    /// Raw audio latency byte (0 unknown, 255 no audio, else `(n - 1) * 2` ms).
    pub audio_latency: Option<u8>,
    /// Raw interlaced audio latency byte.
    pub interlaced_audio_latency: Option<u8>,
}

impl HdmiVsdb {
    /// Audio latency in milliseconds, when the sink reports a usable value.
    pub fn audio_latency_ms(&self) -> Option<u16> {
        match self.audio_latency {
            Some(n @ 1..=251) => Some((u16::from(n) - 1) * 2),
            _ => None,
        }
    }
}

/// The HDMI Forum vendor-specific data block (OUI C4-5D-D8), HDMI 2.0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HdmiForumVsdb {
    pub version: u8,
    /// Maximum TMDS character rate in MHz (5 MHz units in the block).
    pub max_tmds_mhz: u16,
    /// `SCDC_Present` (bit 7 of the byte after the TMDS rate).
    pub scdc_present: bool,
}

/// Why an extension was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CeaError {
    /// Not 128 bytes or not tag 0x02.
    NotCea,
    /// Revision 0 (reserved).
    BadRevision,
    /// `d` points into the header or past the block.
    BadOffset,
}

/// A parsed CEA-861 extension (possibly the merge of several).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cea {
    pub revision: u8,
    pub underscan: bool,
    pub basic_audio: bool,
    pub ycbcr444: bool,
    pub ycbcr422: bool,
    pub svds: Vec<Svd>,
    pub sads: Vec<Sad>,
    /// Speaker allocation byte: bit 0 FL/FR, 1 LFE, 2 FC, 3 RL/RR, 4 RC,
    /// 5 FLC/FRC, 6 RLC/RRC.
    pub speakers: Option<u8>,
    pub hdmi: Option<HdmiVsdb>,
    pub hdmi_forum: Option<HdmiForumVsdb>,
    /// Extended-tag data blocks seen, by extended tag (not decoded).
    pub extended_tags: Vec<u8>,
    /// Detailed timings after the data blocks.
    pub timings: Vec<Timing>,
    /// A data block ran past `d` and the walk stopped early.
    pub malformed: bool,
}

impl Cea {
    /// Fold a second CEA extension into the first.
    pub fn merge(&mut self, other: Cea) {
        self.svds.extend(other.svds);
        self.sads.extend(other.sads);
        self.timings.extend(other.timings);
        self.extended_tags.extend(other.extended_tags);
        self.speakers = self.speakers.or(other.speakers);
        self.hdmi = self.hdmi.or(other.hdmi);
        self.hdmi_forum = self.hdmi_forum.or(other.hdmi_forum);
        self.malformed |= other.malformed;
    }

    /// The most channels any LPCM descriptor offers (0 if none).
    pub fn max_lpcm_channels(&self) -> u8 {
        self.sads
            .iter()
            .filter(|s| s.is_lpcm())
            .map(|s| s.channels)
            .max()
            .unwrap_or(0)
    }
}

fn hdmi_vsdb(p: &[u8]) -> Option<HdmiVsdb> {
    // p[0..3] is the OUI, checked by the caller; p[3..5] the physical address.
    let phys = p.get(3..5)?;
    let flags = p.get(5).copied().unwrap_or(0);
    let latency_flags = p.get(7).copied().unwrap_or(0);
    let (mut audio, mut ilv_audio) = (None, None);
    if latency_flags & 0x80 != 0 {
        audio = p.get(9).copied();
        if latency_flags & 0x40 != 0 {
            ilv_audio = p.get(11).copied();
        }
    }
    Some(HdmiVsdb {
        phys_addr: u16::from_be_bytes([phys[0], phys[1]]),
        supports_ai: flags & 0x80 != 0,
        dc_48bit: flags & 0x40 != 0,
        dc_36bit: flags & 0x20 != 0,
        dc_30bit: flags & 0x10 != 0,
        dc_y444: flags & 0x08 != 0,
        dvi_dual: flags & 0x01 != 0,
        max_tmds_mhz: p.get(6).map(|b| u16::from(*b) * 5),
        audio_latency: audio,
        interlaced_audio_latency: ilv_audio,
    })
}

fn data_block(cea: &mut Cea, tag: u8, p: &[u8]) {
    match tag {
        1 => cea.sads.extend(
            p.chunks_exact(3)
                .map(|c| Sad::from_bytes([c[0], c[1], c[2]])),
        ),
        2 => cea.svds.extend(p.iter().map(|&b| {
            // 1..=64 and 129..=192 carry the native flag in bit 7.
            let native_range = (1..=64).contains(&(b & 0x7F)) && b & 0x80 != 0;
            Svd {
                vic: if native_range || (1..=64).contains(&b) {
                    b & 0x7F
                } else {
                    b
                },
                native: native_range,
            }
        })),
        3 => match p.get(..3) {
            Some([0x03, 0x0C, 0x00]) => {
                if cea.hdmi.is_none() {
                    cea.hdmi = hdmi_vsdb(p);
                }
            }
            Some([0xD8, 0x5D, 0xC4]) if p.len() >= 6 => {
                cea.hdmi_forum = Some(HdmiForumVsdb {
                    version: p[3],
                    max_tmds_mhz: u16::from(p[4]) * 5,
                    scdc_present: p[5] & 0x80 != 0,
                });
            }
            _ => {}
        },
        4 => {
            if let Some(b) = p.first() {
                cea.speakers = Some(*b);
            }
        }
        7 => {
            if let Some(t) = p.first() {
                cea.extended_tags.push(*t);
            }
        }
        _ => {}
    }
}

/// Parse a 128-byte CEA-861 extension block (checksum already verified by
/// the caller, as [`crate::edid::Edid::parse`] does).
pub fn parse(block: &[u8]) -> Result<Cea, CeaError> {
    if block.len() < 128 || block[0] != 0x02 {
        return Err(CeaError::NotCea);
    }
    let revision = block[1];
    if revision == 0 {
        return Err(CeaError::BadRevision);
    }
    let d = usize::from(block[2]);
    if d != 0 && !(4..=127).contains(&d) {
        return Err(CeaError::BadOffset);
    }
    let flags = block[3];
    let mut cea = Cea {
        revision,
        underscan: flags & 0x80 != 0,
        basic_audio: flags & 0x40 != 0,
        ycbcr444: flags & 0x20 != 0,
        ycbcr422: flags & 0x10 != 0,
        ..Cea::default()
    };
    // Revision 1 has no data blocks; d == 0 means neither blocks nor DTDs.
    if revision >= 2 && d != 0 {
        let mut i = 4;
        while i < d {
            let tag = block[i] >> 5;
            let len = usize::from(block[i] & 0x1F);
            let end = i + 1 + len;
            if end > d {
                cea.malformed = true;
                break;
            }
            data_block(&mut cea, tag, &block[i + 1..end]);
            i = end;
        }
    }
    if d != 0 {
        let mut at = d;
        while at + 18 <= 127 {
            match Timing::parse(&block[at..at + 18]) {
                Some(t) => cea.timings.push(t),
                None => break,
            }
            at += 18;
        }
    }
    Ok(cea)
}
