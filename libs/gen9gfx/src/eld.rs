//! The EDID-like data (ELD) the display side hands to the audio codec.
//!
//! The HDMI pin widget of the Intel HDA codec exposes the ELD of the sink on
//! the active port (HDA specification 1.0a section 7.3.3.34, "ELD data", and
//! CTA-861's baseline ELD). `sndd` reads it to learn the sink's rates and
//! channel counts; `gpud` *builds* one from the EDID and writes it to the
//! display engine's ELD registers, which is what makes the pin's ELD valid.
//!
//! Layout, byte by byte (HDA spec table "ELD memory structure"):
//!
//! | byte | content |
//! |---|---|
//! | 0 | bits 7:3 ELD version (2 = CEA-861D baseline, 31 = vendor/partial) |
//! | 1 | vendor-specific version |
//! | 2 | baseline length in 4-byte units |
//! | 3 | reserved |
//! | 4 | bits 7:5 CEA EDID version, bits 4:0 monitor name length (MNL) |
//! | 5 | bits 7:4 SAD count, 3:2 connection type (0 HDMI, 1 DP), 1 S_AI, 0 HDCP |
//! | 6 | audio synchronization delay in 2 ms units |
//! | 7 | speaker allocation (CEA SPK byte) |
//! | 8..16 | port id |
//! | 16..18 | manufacturer name (the EDID bytes 8, 9 as sent) |
//! | 18..20 | product code (the EDID bytes 10, 11) |
//! | 20.. | monitor name (MNL bytes), then SAD count x 3-byte SADs |

use alloc::string::String;
use alloc::vec::Vec;

use crate::cea::{HdmiVsdb, Sad, SAD_RATES_HZ};
use crate::edid::Edid;

/// Bytes before the baseline block.
pub const HEADER_BYTES: usize = 4;
/// Fixed bytes of the baseline block before the monitor name.
pub const BASELINE_FIXED: usize = 16;
/// The longest ELD the HDA codec exposes.
pub const MAX_BYTES: usize = 256;
/// `ELD_Ver` of a CEA-861D baseline ELD.
pub const VER_CEA_861D: u8 = 2;
/// `ELD_Ver` of a vendor-specific ("partial") ELD, accepted but flagged.
pub const VER_PARTIAL: u8 = 31;

/// The connection type field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnType {
    Hdmi,
    DisplayPort,
    Reserved(u8),
}

impl ConnType {
    fn bits(self) -> u8 {
        match self {
            ConnType::Hdmi => 0,
            ConnType::DisplayPort => 1,
            ConnType::Reserved(b) => b & 3,
        }
    }

    fn from_bits(b: u8) -> ConnType {
        match b & 3 {
            0 => ConnType::Hdmi,
            1 => ConnType::DisplayPort,
            r => ConnType::Reserved(r),
        }
    }
}

/// Why an ELD was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EldError {
    /// Shorter than the 4-byte header plus the fixed baseline.
    TooShort,
    /// `ELD_Ver` is neither 2 nor 31 (an invalid ELD reads as version 0).
    BadVersion(u8),
    /// The baseline length runs past the data or past 256 bytes.
    Truncated,
    /// The monitor name and descriptors do not fit the baseline length.
    BadLayout,
}

/// A parsed ELD.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Eld {
    pub version: u8,
    pub vendor_version: u8,
    pub cea_edid_version: u8,
    pub conn_type: ConnType,
    pub supports_ai: bool,
    pub hdcp: bool,
    /// Raw `Audio_Synch_Delay` byte (2 ms units).
    pub audio_sync_delay: u8,
    /// Speaker allocation byte (same bits as [`crate::cea::Cea::speakers`]).
    pub speakers: u8,
    pub port_id: [u8; 8],
    /// EDID bytes 8 and 9 as sent (big endian PNP id).
    pub manufacturer: [u8; 2],
    pub product_code: u16,
    pub monitor_name: String,
    pub sads: Vec<Sad>,
}

impl Eld {
    /// Parse an ELD blob (the codec's read-out, trailing bytes ignored).
    pub fn parse(data: &[u8]) -> Result<Eld, EldError> {
        if data.len() < HEADER_BYTES + BASELINE_FIXED {
            return Err(EldError::TooShort);
        }
        let version = data[0] >> 3;
        if version != VER_CEA_861D && version != VER_PARTIAL {
            return Err(EldError::BadVersion(version));
        }
        let total = HEADER_BYTES + usize::from(data[2]) * 4;
        if total > data.len() || total > MAX_BYTES {
            return Err(EldError::Truncated);
        }
        let mnl = usize::from(data[4] & 0x1F);
        let sad_count = usize::from(data[5] >> 4);
        let sads_at = HEADER_BYTES + BASELINE_FIXED + mnl;
        if mnl > 16 || sads_at + sad_count * 3 > total {
            return Err(EldError::BadLayout);
        }
        let name: String = data[HEADER_BYTES + BASELINE_FIXED..sads_at]
            .iter()
            .take_while(|b| **b != 0)
            .map(|b| if (0x20..0x7F).contains(b) { *b as char } else { '?' })
            .collect();
        let mut port_id = [0u8; 8];
        port_id.copy_from_slice(&data[8..16]);
        Ok(Eld {
            version,
            vendor_version: data[1],
            cea_edid_version: data[4] >> 5,
            conn_type: ConnType::from_bits(data[5] >> 2),
            supports_ai: data[5] & 2 != 0,
            hdcp: data[5] & 1 != 0,
            audio_sync_delay: data[6],
            speakers: data[7],
            port_id,
            manufacturer: [data[16], data[17]],
            product_code: u16::from_le_bytes([data[18], data[19]]),
            monitor_name: name,
            sads: data[sads_at..sads_at + sad_count * 3]
                .chunks_exact(3)
                .map(|c| Sad::from_bytes([c[0], c[1], c[2]]))
                .collect(),
        })
    }

    /// Whether this is a partial/vendor-specific ELD (content may be thin).
    pub fn is_partial(&self) -> bool {
        self.version == VER_PARTIAL
    }

    /// Build the ELD the way a display driver does from an EDID: its monitor
    /// name (at most 16 bytes), at most 15 SADs, the CEA speaker allocation,
    /// the HDMI vendor block's AI flag and audio latency. `None` when the
    /// EDID has no CEA extension, since there is then nothing to say about
    /// audio.
    pub fn from_edid(edid: &Edid, conn_type: ConnType, port_id: [u8; 8]) -> Option<Eld> {
        let cea = edid.cea.as_ref()?;
        let [a, b, c] = edid.manufacturer.map(|l| u16::from(l.wrapping_sub(b'A' - 1)) & 0x1F);
        let mfg = (a << 10 | b << 5 | c).to_be_bytes();
        let mut name = String::new();
        if let Some(n) = &edid.monitor_name {
            name.extend(n.chars().take(16));
        }
        let hdmi: Option<&HdmiVsdb> = cea.hdmi.as_ref();
        let delay = hdmi
            .and_then(|h| h.audio_latency)
            .filter(|n| (1..=251).contains(n))
            .map_or(0, |n| n - 1);
        Some(Eld {
            version: VER_CEA_861D,
            vendor_version: 0,
            cea_edid_version: cea.revision & 7,
            conn_type,
            supports_ai: hdmi.is_some_and(|h| h.supports_ai),
            hdcp: false,
            audio_sync_delay: delay,
            speakers: cea.speakers.unwrap_or(0),
            port_id,
            manufacturer: mfg,
            product_code: edid.product_code,
            monitor_name: name,
            sads: cea.sads.iter().copied().take(15).collect(),
        })
    }

    /// Serialise: the 4-byte header and a baseline block padded to 4 bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let name: Vec<u8> = self.monitor_name.bytes().take(16).collect();
        let sads = &self.sads[..self.sads.len().min(15)];
        let baseline = (BASELINE_FIXED + name.len() + sads.len() * 3).next_multiple_of(4);
        let mut out = alloc::vec![0u8; HEADER_BYTES + baseline];
        out[0] = (self.version & 0x1F) << 3;
        out[1] = self.vendor_version;
        out[2] = (baseline / 4) as u8;
        out[4] = (self.cea_edid_version & 7) << 5 | name.len() as u8;
        out[5] = (sads.len() as u8) << 4
            | self.conn_type.bits() << 2
            | u8::from(self.supports_ai) << 1
            | u8::from(self.hdcp);
        out[6] = self.audio_sync_delay;
        out[7] = self.speakers;
        out[8..16].copy_from_slice(&self.port_id);
        out[16..18].copy_from_slice(&self.manufacturer);
        out[18..20].copy_from_slice(&self.product_code.to_le_bytes());
        let mut at = HEADER_BYTES + BASELINE_FIXED;
        out[at..at + name.len()].copy_from_slice(&name);
        at += name.len();
        for sad in sads {
            out[at..at + 3].copy_from_slice(&sad.to_bytes());
            at += 3;
        }
        out
    }

    pub fn audio_sync_delay_ms(&self) -> u16 {
        u16::from(self.audio_sync_delay) * 2
    }

    /// Union of the LPCM descriptors' rate masks (bit `i` is
    /// [`SAD_RATES_HZ`]`[i]`).
    pub fn lpcm_rates(&self) -> u8 {
        self.sads
            .iter()
            .filter(|s| s.is_lpcm())
            .fold(0, |m, s| m | s.rates)
    }

    /// The LPCM rates in Hz, ascending.
    pub fn lpcm_rates_hz(&self) -> Vec<u32> {
        let mask = self.lpcm_rates();
        SAD_RATES_HZ
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, r)| *r)
            .collect()
    }

    /// The most LPCM channels any descriptor offers.
    pub fn max_lpcm_channels(&self) -> u8 {
        self.sads
            .iter()
            .filter(|s| s.is_lpcm())
            .map(|s| s.channels)
            .max()
            .unwrap_or(0)
    }

    /// Whether the sink accepts `channels` of LPCM at `rate_hz`: a single
    /// descriptor must allow both, as a SAD pairs a channel maximum with its
    /// rates.
    pub fn supports_lpcm(&self, rate_hz: u32, channels: u8) -> bool {
        self.sads
            .iter()
            .any(|s| s.is_lpcm() && s.channels >= channels && s.supports_rate(rate_hz))
    }
}
