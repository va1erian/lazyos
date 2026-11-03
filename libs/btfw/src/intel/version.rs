//! The Intel Read_Version answers (vendor command `0xFC05`) and the firmware
//! file names they select.
//!
//! Two layouts share the opcode. Parts up to "CcP" answer the legacy fixed
//! record (ten octets, status first, hardware platform `0x37` second). Newer
//! parts, and the operational firmware of the middle generations, answer a
//! list of type-length-value entries when the command carries the parameter
//! `0xFF`. The layouts and the naming are the documented behaviour of
//! Linux's `drivers/bluetooth/btintel.c` (`btintel_read_version`,
//! `btintel_parse_version_tlv`, `btintel_get_fw_name`,
//! `btintel_get_fw_name_tlv`); this is an independent, bounds-checked
//! implementation.

use alloc::format;
use alloc::string::String;

/// The hardware platform octet every Intel iBT part reports.
pub(crate) const HW_PLATFORM: u8 = 0x37;
/// `fw_variant` of the legacy bootloader / operational firmware.
pub(crate) const FW_BOOTLOADER: u8 = 0x06;
pub(crate) const FW_OPERATIONAL: u8 = 0x23;
/// `img_type` of a TLV answer.
pub(crate) const IMG_BOOTLOADER: u8 = 0x01;
pub(crate) const IMG_IML: u8 = 0x02;
pub(crate) const IMG_OPERATIONAL: u8 = 0x03;

/// The legacy record, status octet removed (nine octets).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Legacy {
    pub hw_variant: u8,
    pub hw_revision: u8,
    pub fw_variant: u8,
    pub fw_revision: u8,
    /// Build number, week, year: compared with the file's boot parameters.
    pub fw_build: (u8, u8, u8),
}

/// The legacy answer (status included) is exactly ten octets with the
/// platform octet second; anything else is TLV.
pub(crate) fn is_legacy(return_params: &[u8]) -> bool {
    return_params.len() == 10 && return_params[1] == HW_PLATFORM
}

pub(crate) fn parse_legacy(return_params: &[u8]) -> Option<Legacy> {
    if !is_legacy(return_params) {
        return None;
    }
    Some(Legacy {
        hw_variant: return_params[2],
        hw_revision: return_params[3],
        fw_variant: return_params[4],
        fw_revision: return_params[5],
        fw_build: (return_params[6], return_params[7], return_params[8]),
    })
}

/// What the legacy bootloader's boot parameters (`0xFC0D`) tell us.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BootParams {
    pub dev_revid: u16,
    /// Non-zero: the bootloader does not acknowledge every fragment, which the
    /// download below depends on.
    pub limited_cce: u8,
}

/// Status plus 22 octets of parameters.
const BOOT_PARAMS_LEN: usize = 23;

pub(crate) fn parse_boot_params(return_params: &[u8]) -> Option<BootParams> {
    if return_params.len() < BOOT_PARAMS_LEN {
        return None;
    }
    Some(BootParams {
        dev_revid: u16::from_le_bytes([return_params[4], return_params[5]]),
        limited_cce: return_params[21],
    })
}

/// The firmware name of a legacy bootloader part, or `None` for a hardware
/// variant with no naming rule here. `0x0b` and `0x0c` are named by the device
/// revision from the boot parameters, `0x11` to `0x14` by the version record.
pub(crate) fn legacy_name(ver: &Legacy, params: &BootParams, suffix: &str) -> Option<String> {
    match ver.hw_variant {
        0x0b | 0x0c => Some(format!(
            "intel/ibt-{}-{}.{suffix}",
            ver.hw_variant, params.dev_revid
        )),
        0x11..=0x14 => Some(format!(
            "intel/ibt-{}-{}-{}.{suffix}",
            ver.hw_variant, ver.hw_revision, ver.fw_revision
        )),
        _ => None,
    }
}

/// The TLV answer, the entries this loader uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tlv {
    pub cnvi_top: u32,
    pub cnvr_top: u32,
    pub cnvi_bt: u32,
    pub img_type: u8,
    pub sbe_type: u8,
    /// Minimum firmware build number, calendar week and year.
    pub min_fw: (u8, u8, u8),
}

const T_CNVI_TOP: u8 = 0x10;
const T_CNVR_TOP: u8 = 0x11;
const T_CNVI_BT: u8 = 0x12;
const T_IMAGE_TYPE: u8 = 0x1c;
const T_MIN_FW: u8 = 0x2d;
const T_SBE_TYPE: u8 = 0x2f;

impl Tlv {
    /// The hardware variant: bits 16 to 21 of `cnvi_bt`.
    pub fn hw_variant(&self) -> u8 {
        ((self.cnvi_bt >> 16) & 0x3f) as u8
    }

    pub fn hw_platform(&self) -> u8 {
        ((self.cnvi_bt >> 8) & 0xff) as u8
    }
}

/// Parse the entries after the status octet. Every entry's length is checked
/// against the bytes present and against the minimum its type needs; an entry
/// of an unknown type is skipped. The four entries the file naming needs must
/// all be there.
pub(crate) fn parse_tlv(mut rest: &[u8]) -> Result<Tlv, &'static str> {
    let mut tlv = Tlv::default();
    let mut seen = 0u8;
    while !rest.is_empty() {
        let [ty, len, tail @ ..] = rest else {
            return Err("truncated TLV header");
        };
        let len = usize::from(*len);
        if tail.len() < len {
            return Err("TLV value runs past the answer");
        }
        let (val, next) = tail.split_at(len);
        match *ty {
            T_CNVI_TOP => (tlv.cnvi_top, seen) = (u32_of(val)?, seen | 1),
            T_CNVR_TOP => (tlv.cnvr_top, seen) = (u32_of(val)?, seen | 2),
            T_CNVI_BT => (tlv.cnvi_bt, seen) = (u32_of(val)?, seen | 4),
            T_IMAGE_TYPE => (tlv.img_type, seen) = (u8_of(val)?, seen | 8),
            T_SBE_TYPE => tlv.sbe_type = u8_of(val)?,
            T_MIN_FW => match val {
                [nn, cw, yy, ..] => tlv.min_fw = (*nn, *cw, *yy),
                _ => return Err("short minimum-firmware entry"),
            },
            _ => {}
        }
        rest = next;
    }
    if seen == 0xf {
        Ok(tlv)
    } else {
        Err("version answer lacks a required entry")
    }
}

fn u32_of(val: &[u8]) -> Result<u32, &'static str> {
    match val {
        [a, b, c, d, ..] => Ok(u32::from_le_bytes([*a, *b, *c, *d])),
        _ => Err("short 32-bit entry"),
    }
}

fn u8_of(val: &[u8]) -> Result<u8, &'static str> {
    val.first().copied().ok_or("empty entry")
}

/// `ibt-<cnvi>-<cnvr>.<suffix>` where each half packs the CNVx type (12 bits)
/// and step (4 bits) byte-swapped, so the type `0x410` at step 0 reads `0041`.
pub(crate) fn tlv_name(tlv: &Tlv, suffix: &str) -> String {
    format!(
        "intel/ibt-{:04x}-{:04x}.{suffix}",
        pack(tlv.cnvi_top),
        pack(tlv.cnvr_top)
    )
}

fn pack(top: u32) -> u16 {
    let kind = (top & 0xfff) as u16;
    let step = ((top >> 24) & 0xf) as u16;
    (kind << 4 | step).swap_bytes()
}
