//! EDID 1.x base block (VESA E-EDID 1.4) and the extension list.
//!
//! Input is whatever a monitor sent over DDC: any length, any content. The
//! parser validates the header and checksums, never indexes past the slice
//! and keeps what it could read: a bad *extension* costs only that extension
//! (counted in [`Edid::bad_extensions`]), a bad base block is an error.

use alloc::string::String;
use alloc::vec::Vec;

use crate::cea::{self, Cea};

/// One EDID block.
pub const BLOCK: usize = 128;
/// The fixed 8-byte header of a base block.
pub const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
/// Extension tag of a CEA-861 extension.
pub const EXT_CEA: u8 = 0x02;

/// Why a base block was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdidError {
    /// Fewer than 128 bytes.
    TooShort,
    /// The 8-byte header is wrong.
    BadHeader,
    /// Block 0's bytes do not sum to 0 mod 256.
    BadChecksum,
    /// Not EDID version 1.
    BadVersion,
}

/// A detailed timing descriptor (also used for CEA extension DTDs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    pub pixel_clock_khz: u32,
    pub h_active: u16,
    pub h_blank: u16,
    pub h_front: u16,
    pub h_sync: u16,
    pub v_active: u16,
    pub v_blank: u16,
    pub v_front: u16,
    pub v_sync: u16,
    pub h_size_mm: u16,
    pub v_size_mm: u16,
    pub interlaced: bool,
    pub h_sync_positive: bool,
    pub v_sync_positive: bool,
}

impl Timing {
    /// Parse an 18-byte descriptor. `None` for a display descriptor (pixel
    /// clock 0), a short slice or a timing with an empty active area.
    pub fn parse(d: &[u8]) -> Option<Timing> {
        let d = d.get(..18)?;
        let clock = u32::from(u16::from_le_bytes([d[0], d[1]])) * 10;
        if clock == 0 {
            return None;
        }
        let hi = |b: u8, shift: u32| u16::from((b >> shift) & 0x0F);
        let h_active = u16::from(d[2]) | hi(d[4], 4) << 8;
        let h_blank = u16::from(d[3]) | hi(d[4], 0) << 8;
        let v_active = u16::from(d[5]) | hi(d[7], 4) << 8;
        let v_blank = u16::from(d[6]) | hi(d[7], 0) << 8;
        if h_active == 0 || v_active == 0 {
            return None;
        }
        let flags = d[17];
        // Bits 4:3 == 0b11 is "digital separate sync": bit 2 is vsync
        // polarity, bit 1 hsync polarity. Other sync types carry no usable
        // polarity bits, so they read as negative.
        let separate = flags & 0x18 == 0x18;
        Some(Timing {
            pixel_clock_khz: clock,
            h_active,
            h_blank,
            h_front: u16::from(d[8]) | u16::from(d[11] >> 6) << 8,
            h_sync: u16::from(d[9]) | u16::from((d[11] >> 4) & 3) << 8,
            v_active,
            v_blank,
            v_front: u16::from(d[10] >> 4) | u16::from((d[11] >> 2) & 3) << 4,
            v_sync: u16::from(d[10] & 0x0F) | u16::from(d[11] & 3) << 4,
            h_size_mm: u16::from(d[12]) | hi(d[14], 4) << 8,
            v_size_mm: u16::from(d[13]) | hi(d[14], 0) << 8,
            interlaced: flags & 0x80 != 0,
            h_sync_positive: separate && flags & 0x02 != 0,
            v_sync_positive: separate && flags & 0x04 != 0,
        })
    }

    pub fn h_total(&self) -> u32 {
        u32::from(self.h_active) + u32::from(self.h_blank)
    }

    pub fn v_total(&self) -> u32 {
        u32::from(self.v_active) + u32::from(self.v_blank)
    }

    /// Refresh in millihertz (the field rate for an interlaced timing).
    pub fn refresh_millihz(&self) -> u32 {
        let total = u64::from(self.h_total()) * u64::from(self.v_total());
        if total == 0 {
            return 0;
        }
        (u64::from(self.pixel_clock_khz) * 1_000_000 / total) as u32
    }
}

/// A standard timing (EDID bytes 38 to 53).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StdTiming {
    pub h_active: u16,
    pub v_active: u16,
    pub refresh_hz: u8,
}

/// The monitor range limits descriptor (tag 0xFD).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RangeLimits {
    pub v_min_hz: u16,
    pub v_max_hz: u16,
    pub h_min_khz: u16,
    pub h_max_khz: u16,
    pub max_pixel_clock_mhz: u16,
}

/// A parsed EDID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edid {
    /// The three-letter PNP manufacturer id.
    pub manufacturer: [u8; 3],
    pub product_code: u16,
    pub serial: u32,
    pub week: u8,
    pub year: u16,
    pub version: u8,
    pub revision: u8,
    pub digital: bool,
    pub width_cm: u8,
    pub height_cm: u8,
    pub standard: Vec<StdTiming>,
    /// Detailed timings of the base block, the preferred one first.
    pub detailed: Vec<Timing>,
    pub monitor_name: Option<String>,
    pub monitor_serial: Option<String>,
    pub range: Option<RangeLimits>,
    /// The CEA-861 extension(s), merged.
    pub cea: Option<Cea>,
    /// The base block's extension count (byte 126).
    pub extension_count: u8,
    /// Extensions that were missing, failed their checksum or did not parse.
    pub bad_extensions: u8,
}

/// Sum of a block's bytes mod 256 (0 for a valid block).
pub fn checksum(block: &[u8]) -> u8 {
    block.iter().fold(0u8, |a, b| a.wrapping_add(*b))
}

/// The text of a 0xFC/0xFF/0xFE descriptor: up to 13 bytes, ended by a line
/// feed, trailing blanks trimmed, anything not printable ASCII shown as `?`.
fn descriptor_text(d: &[u8]) -> String {
    let mut s = String::new();
    for &b in d.get(5..18).unwrap_or(&[]) {
        if b == 0x0A {
            break;
        }
        s.push(if (0x20..0x7F).contains(&b) {
            b as char
        } else {
            '?'
        });
    }
    while s.ends_with(' ') {
        s.pop();
    }
    s
}

fn range_limits(d: &[u8]) -> Option<RangeLimits> {
    let d = d.get(..18)?;
    let flags = d[4];
    let (v_add_max, v_add_min) = offsets(flags & 3);
    let (h_add_max, h_add_min) = offsets((flags >> 2) & 3);
    Some(RangeLimits {
        v_min_hz: u16::from(d[5]) + v_add_min,
        v_max_hz: u16::from(d[6]) + v_add_max,
        h_min_khz: u16::from(d[7]) + h_add_min,
        h_max_khz: u16::from(d[8]) + h_add_max,
        max_pixel_clock_mhz: u16::from(d[9]) * 10,
    })
}

/// EDID 1.4 range offset flags: 0b10 adds 255 to the maximum, 0b11 to both.
fn offsets(bits: u8) -> (u16, u16) {
    match bits {
        2 => (255, 0),
        3 => (255, 255),
        _ => (0, 0),
    }
}

fn std_timing(a: u8, b: u8) -> Option<StdTiming> {
    if a == 0x01 && b == 0x01 || a == 0 {
        return None;
    }
    let h = (u16::from(a) + 31) * 8;
    // EDID 1.3 and later: 00 is 16:10 (1.0 to 1.2 meant 1:1).
    let v = match b >> 6 {
        0 => h * 10 / 16,
        1 => h * 3 / 4,
        2 => h * 4 / 5,
        _ => h * 9 / 16,
    };
    Some(StdTiming {
        h_active: h,
        v_active: v,
        refresh_hz: (b & 0x3F) + 60,
    })
}

impl Edid {
    /// Parse an EDID blob of one base block and any extensions that follow.
    pub fn parse(data: &[u8]) -> Result<Edid, EdidError> {
        let base = data.get(..BLOCK).ok_or(EdidError::TooShort)?;
        if base[..8] != HEADER {
            return Err(EdidError::BadHeader);
        }
        if checksum(base) != 0 {
            return Err(EdidError::BadChecksum);
        }
        if base[18] != 1 {
            return Err(EdidError::BadVersion);
        }
        let m = u16::from_be_bytes([base[8], base[9]]);
        let letter = |v: u16| b'A' - 1 + (v & 0x1F) as u8;
        let mut edid = Edid {
            manufacturer: [letter(m >> 10), letter(m >> 5), letter(m)],
            product_code: u16::from_le_bytes([base[10], base[11]]),
            serial: u32::from_le_bytes([base[12], base[13], base[14], base[15]]),
            week: base[16],
            year: 1990 + u16::from(base[17]),
            version: base[18],
            revision: base[19],
            digital: base[20] & 0x80 != 0,
            width_cm: base[21],
            height_cm: base[22],
            standard: Vec::new(),
            detailed: Vec::new(),
            monitor_name: None,
            monitor_serial: None,
            range: None,
            cea: None,
            extension_count: base[126],
            bad_extensions: 0,
        };
        for pair in base[38..54].chunks_exact(2) {
            edid.standard.extend(std_timing(pair[0], pair[1]));
        }
        for d in base[54..126].chunks_exact(18) {
            if d[0] != 0 || d[1] != 0 {
                edid.detailed.extend(Timing::parse(d));
                continue;
            }
            match d[3] {
                0xFC => edid.monitor_name = Some(descriptor_text(d)),
                0xFF => edid.monitor_serial = Some(descriptor_text(d)),
                0xFD => edid.range = range_limits(d),
                _ => {}
            }
        }
        for n in 1..=usize::from(edid.extension_count) {
            let Some(block) = data.get(n * BLOCK..(n + 1) * BLOCK) else {
                edid.bad_extensions += 1;
                continue;
            };
            if checksum(block) != 0 {
                edid.bad_extensions += 1;
                continue;
            }
            if block[0] != EXT_CEA {
                // Extension maps (0xF0), DisplayID and others: not needed.
                continue;
            }
            match cea::parse(block) {
                Ok(c) => match &mut edid.cea {
                    Some(first) => first.merge(c),
                    None => edid.cea = Some(c),
                },
                Err(_) => edid.bad_extensions += 1,
            }
        }
        Ok(edid)
    }

    /// The preferred timing: the first detailed timing of the base block.
    pub fn preferred(&self) -> Option<&Timing> {
        self.detailed.first()
    }

    /// The manufacturer id as a string ("DEL").
    pub fn manufacturer_str(&self) -> String {
        self.manufacturer.iter().map(|b| *b as char).collect()
    }

    /// Whether the sink advertises HDMI (a CEA extension carrying the HDMI
    /// vendor block), which is what decides whether data islands and audio
    /// can be used; a DVI monitor has no such block.
    pub fn is_hdmi(&self) -> bool {
        self.cea.as_ref().is_some_and(|c| c.hdmi.is_some())
    }

    /// Whether audio is advertised: a short audio descriptor, or the CEA
    /// "basic audio" flag of an HDMI sink.
    pub fn has_audio(&self) -> bool {
        self.is_hdmi()
            && self
                .cea
                .as_ref()
                .is_some_and(|c| !c.sads.is_empty() || c.basic_audio)
    }
}
