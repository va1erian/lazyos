//! The Video BIOS Table (VBT): header, BIOS Data Block (BDB) block walk and
//! the child device list that names each display port.
//!
//! Where it comes from: on UEFI the table sits in the ACPI OpRegion whose
//! physical address is in the GPU's `ASLS` config dword (offset 0xFC); the
//! legacy option ROM holds a copy. Linux exposes it as the debugfs blob
//! `i915_vbt`, which `tools/boot/survey_gpu.sh` saves.
//!
//! Every layout below is from memory of i915's `intel_vbt_defs.h`. Where the
//! source documents a field as added in a given BDB version ("158+", "192+")
//! the version gate is kept. Anything I am not sure of carries
//! `// TO CONFIRM` and should be settled against the box's own `vbt.bin`
//! (`intel_vbt_decode` output is the cross-check).

use alloc::vec::Vec;

/// Size of `struct vbt_header` (signature 20, version 2, header size 2, vbt
/// size 2, checksum 1, reserved 1, bdb offset 4, aim offsets 4 x 4).
pub const VBT_HEADER_SIZE: usize = 48;
/// Size of `struct bdb_header` (signature 16, version 2, header size 2, bdb
/// size 2).
pub const BDB_HEADER_SIZE: usize = 22;
const VBT_SIGNATURE: &[u8] = b"$VBT";
const BDB_SIGNATURE: &[u8] = b"BIOS_DATA_BLOCK ";

/// BDB block ids this module knows by name.
pub mod block {
    pub const GENERAL_FEATURES: u8 = 1;
    pub const GENERAL_DEFINITIONS: u8 = 2;
    pub const DRIVER_FEATURES: u8 = 12;
    pub const EDP: u8 = 27;
    pub const LVDS_OPTIONS: u8 = 40;
    /// "Skip this block": its size is valid and its content is ignored.
    pub const SKIP: u8 = 254;
}

/// `device_type` bits (i915 `DEVICE_TYPE_*`).
pub mod device_type {
    pub const ANALOG_OUTPUT: u16 = 1 << 0;
    pub const DISPLAYPORT_OUTPUT: u16 = 1 << 2;
    pub const TMDS_DVI_SIGNALING: u16 = 1 << 4;
    pub const NOT_HDMI_OUTPUT: u16 = 1 << 11;
    pub const INTERNAL_CONNECTOR: u16 = 1 << 12;
    /// Common whole values: HDMI, DP, DP with dual mode (DP++), eDP.
    pub const HDMI: u16 = 0x60D2;
    pub const DP: u16 = 0x68C6;
    pub const DP_DUAL_MODE: u16 = 0x60D6;
    pub const EDP: u16 = 0x78C6;
}

/// `dvo_port` values (i915 `DVO_PORT_*`).
pub mod dvo_port {
    pub const HDMIA: u8 = 0;
    pub const HDMIB: u8 = 1;
    pub const HDMIC: u8 = 2;
    pub const HDMID: u8 = 3;
    pub const LVDS: u8 = 4;
    pub const TV: u8 = 5;
    pub const CRT: u8 = 6;
    pub const DPB: u8 = 7;
    pub const DPC: u8 = 8;
    pub const DPD: u8 = 9;
    pub const DPA: u8 = 10;
    pub const DPE: u8 = 11;
    pub const HDMIE: u8 = 12;
}

/// A DDI port (Gen9 has A to E; A is eDP/DP only on most boards).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Port {
    A,
    B,
    C,
    D,
    E,
}

impl Port {
    pub fn from_dvo_port(p: u8) -> Option<Port> {
        Some(match p {
            dvo_port::HDMIA | dvo_port::DPA => Port::A,
            dvo_port::HDMIB | dvo_port::DPB => Port::B,
            dvo_port::HDMIC | dvo_port::DPC => Port::C,
            dvo_port::HDMID | dvo_port::DPD => Port::D,
            dvo_port::HDMIE | dvo_port::DPE => Port::E,
            _ => return None,
        })
    }
}

/// What kind of output a child device describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputKind {
    /// TMDS with HDMI signalling (data islands, audio).
    Hdmi,
    /// TMDS, DVI only (the VBT says "not an HDMI output").
    Dvi,
    DisplayPort,
    /// DP++ port: DisplayPort and TMDS (dual mode) on the same connector.
    DpDualMode,
    Edp,
    Crt,
    Lvds,
    Other,
}

/// Why a VBT was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VbtError {
    TooShort,
    /// Does not start with `$VBT`.
    BadSignature,
    /// `vbt_size` is smaller than the header or larger than the data.
    BadSize,
    /// The BDB offset points outside the table.
    BadBdbOffset,
    /// The BDB header does not carry `BIOS_DATA_BLOCK `.
    BadBdbSignature,
}

fn u16le(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*d.get(at)?, *d.get(at + 1)?]))
}

/// One child device (a display the firmware says may be attached).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Child {
    pub handle: u16,
    pub device_type: u16,
    pub dvo_port: u8,
    /// The DDI port, when `dvo_port` names one.
    pub port: Option<Port>,
    pub kind: OutputKind,
    /// DDC pin for EDID (a legacy VBT value, see [`gmbus_pin`]).
    pub ddc_pin: u8,
    pub i2c_pin: u8,
    /// DP AUX channel selector (0x40 A, 0x10 B, 0x20 C, 0x30 D, 0x50 E).
    pub aux_channel: u8,
    /// A level shifter value for HDMI buffer translations
    /// (`// TO CONFIRM`: bits 4:0 of device-union byte 7, offset 7).
    pub hdmi_level_shifter: u8,
    /// BDB 192+: a DP port that carries an LSPCON (DP to HDMI 2.0
    /// converter): HDMI needs AUX and a PCON mode switch.
    pub lspcon: bool,
    /// BDB 184+.
    pub lane_reversal: bool,
    /// BDB 158+ capability flags (derived from `device_type` before that).
    pub hdmi_support: bool,
    pub dp_support: bool,
    pub tmds_support: bool,
}

impl Child {
    /// Whether HDMI can be driven from this port: native TMDS, DP++ or an
    /// LSPCON on a DP port.
    pub fn can_hdmi(&self) -> bool {
        matches!(self.kind, OutputKind::Hdmi | OutputKind::DpDualMode)
            || (self.kind == OutputKind::DisplayPort && self.lspcon)
    }

    fn parse(d: &[u8], bdb_version: u16) -> Option<Child> {
        let device_type = u16le(d, 2)?;
        if device_type == 0 {
            return None;
        }
        let byte = |i: usize| d.get(i).copied().unwrap_or(0);
        let dvo = byte(16);
        let is_dvi = device_type & device_type::TMDS_DVI_SIGNALING != 0;
        let is_dp = device_type & device_type::DISPLAYPORT_OUTPUT != 0;
        let is_crt = device_type & device_type::ANALOG_OUTPUT != 0;
        let is_hdmi = is_dvi && device_type & device_type::NOT_HDMI_OUTPUT == 0;
        let is_edp = is_dp && device_type & device_type::INTERNAL_CONNECTOR != 0;
        let kind = if is_dp && is_dvi {
            OutputKind::DpDualMode
        } else if is_edp {
            OutputKind::Edp
        } else if is_dp {
            OutputKind::DisplayPort
        } else if is_hdmi {
            OutputKind::Hdmi
        } else if is_dvi {
            OutputKind::Dvi
        } else if is_crt {
            OutputKind::Crt
        } else if dvo == dvo_port::LVDS {
            OutputKind::Lvds
        } else {
            OutputKind::Other
        };
        // Bytes 23 and 24 are flag bytes from BDB 158 (dvo2_port and i2c2_pin
        // before that, which a DDI port never uses).
        let (f23, f24) = if bdb_version >= 158 {
            (byte(23), byte(24))
        } else {
            (0, 0)
        };
        let (hdmi_support, dp_support, tmds_support) = if bdb_version >= 158 {
            (f24 & 1 != 0, f24 & 2 != 0, f24 & 4 != 0)
        } else {
            (is_hdmi, is_dp, is_dvi)
        };
        Some(Child {
            handle: u16le(d, 0)?,
            device_type,
            dvo_port: dvo,
            port: Port::from_dvo_port(dvo),
            kind,
            ddc_pin: byte(19),
            i2c_pin: byte(17),
            aux_channel: if bdb_version >= 158 { byte(25) } else { 0 },
            hdmi_level_shifter: byte(7) & 0x1F,
            lspcon: bdb_version >= 192 && f23 & 0x04 != 0,
            lane_reversal: bdb_version >= 184 && f23 & 0x02 != 0,
            hdmi_support,
            dp_support,
            tmds_support,
        })
    }
}

/// The GMBUS pin a legacy (pre-Broxton, pre-Ice Lake) VBT `ddc_pin` names.
/// `// TO CONFIRM against dump`: from memory of i915's `gmbus_pin` values
/// (SSC 1, VGA 2, panel 3, DPC 4, DPB 5, DPD 6), which VBT writers reuse as
/// the DDC pin of a DDI port on SKL/KBL.
pub fn gmbus_pin(ddc_pin: u8) -> Option<u8> {
    matches!(ddc_pin, 1..=6).then_some(ddc_pin)
}

/// A parsed VBT (borrowing the table bytes).
#[derive(Clone, Copy, Debug)]
pub struct Vbt<'a> {
    data: &'a [u8],
    /// Header version (e.g. 155 for 1.55).
    pub version: u16,
    pub header_size: u16,
    pub vbt_size: u16,
    /// BDB version; the field gates in [`Child`] use it.
    pub bdb_version: u16,
    bdb_start: usize,
    bdb_end: usize,
    blocks_start: usize,
    /// Byte sum of the table is 0 mod 256. `// TO CONFIRM`: the VBT's own
    /// checksum rule; informational only, i915 does not enforce it.
    pub checksum_ok: bool,
}

/// Iterator over `(block id, block data)` of the BDB.
pub struct Blocks<'a> {
    data: &'a [u8],
    at: usize,
    end: usize,
}

impl<'a> Iterator for Blocks<'a> {
    type Item = (u8, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let id = *self.data.get(self.at).filter(|_| self.at + 3 <= self.end)?;
        let size = usize::from(u16le(self.data, self.at + 1)?);
        let start = self.at + 3;
        let stop = start.checked_add(size).filter(|s| *s <= self.end)?;
        self.at = stop;
        Some((id, &self.data[start..stop]))
    }
}

impl<'a> Vbt<'a> {
    /// Parse the headers. The block contents are read lazily.
    pub fn parse(data: &'a [u8]) -> Result<Vbt<'a>, VbtError> {
        if data.len() < VBT_HEADER_SIZE {
            return Err(VbtError::TooShort);
        }
        if &data[..VBT_SIGNATURE.len()] != VBT_SIGNATURE {
            return Err(VbtError::BadSignature);
        }
        let version = u16le(data, 20).ok_or(VbtError::TooShort)?;
        let header_size = u16le(data, 22).ok_or(VbtError::TooShort)?;
        let vbt_size = u16le(data, 24).ok_or(VbtError::TooShort)?;
        let size = usize::from(vbt_size);
        if size < VBT_HEADER_SIZE + BDB_HEADER_SIZE || size > data.len() {
            return Err(VbtError::BadSize);
        }
        let table = &data[..size];
        let bdb_offset = u32::from_le_bytes([data[28], data[29], data[30], data[31]]) as usize;
        let bdb_end_hdr = bdb_offset
            .checked_add(BDB_HEADER_SIZE)
            .filter(|e| *e <= size)
            .ok_or(VbtError::BadBdbOffset)?;
        if &table[bdb_offset..bdb_offset + BDB_SIGNATURE.len()] != BDB_SIGNATURE {
            return Err(VbtError::BadBdbSignature);
        }
        let bdb_version = u16le(table, bdb_offset + 16).ok_or(VbtError::BadBdbOffset)?;
        let bdb_header_size =
            usize::from(u16le(table, bdb_offset + 18).ok_or(VbtError::BadBdbOffset)?);
        let bdb_size = usize::from(u16le(table, bdb_offset + 20).ok_or(VbtError::BadBdbOffset)?);
        Ok(Vbt {
            data: table,
            version,
            header_size,
            vbt_size,
            bdb_version,
            bdb_start: bdb_offset,
            bdb_end: bdb_offset.saturating_add(bdb_size).min(size),
            blocks_start: bdb_offset
                .saturating_add(bdb_header_size.max(BDB_HEADER_SIZE))
                .max(bdb_end_hdr),
            checksum_ok: table.iter().fold(0u8, |a, b| a.wrapping_add(*b)) == 0,
        })
    }

    /// Offset of the BDB inside the table.
    pub fn bdb_offset(&self) -> usize {
        self.bdb_start
    }

    /// Every block of the BDB, stopping at the first one that runs off the
    /// end (a truncated or damaged table keeps what precedes it).
    pub fn blocks(&self) -> Blocks<'a> {
        Blocks {
            data: self.data,
            at: self.blocks_start,
            end: self.bdb_end,
        }
    }

    /// The first block with this id.
    pub fn block(&self, id: u8) -> Option<&'a [u8]> {
        self.blocks().find(|(i, _)| *i == id).map(|(_, d)| d)
    }

    /// The child devices of the General Definitions block, skipping empty
    /// entries (`device_type == 0`). At most 32 are read.
    pub fn children(&self) -> Vec<Child> {
        let Some(defs) = self.block(block::GENERAL_DEFINITIONS) else {
            return Vec::new();
        };
        // crt_ddc_gmbus_pin, dpms bits, boot_display[2], child_dev_size, then
        // the devices.
        let size = usize::from(defs.get(4).copied().unwrap_or(0));
        let Some(devices) = defs.get(5..).filter(|_| size >= 20) else {
            return Vec::new();
        };
        devices
            .chunks_exact(size)
            .take(32)
            .filter_map(|d| Child::parse(d, self.bdb_version))
            .collect()
    }

    /// The child on a DDI port that can drive HDMI, if any.
    pub fn hdmi_child(&self) -> Option<Child> {
        self.children().into_iter().find(Child::can_hdmi)
    }
}
