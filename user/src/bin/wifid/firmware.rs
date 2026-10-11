//! The firmware file (`rtw88::FIRMWARE` under `fhs::FIRMWARE`), loaded
//! through `libs/fwload`, and the version fields of its header for the
//! evidence line. Parsing the image for download is `libs/rtw88`'s `fw`
//! module; this reads only fixed offsets of upstream's `struct rtw_fw_hdr`
//! (`fw.h`), each bounds-checked, to say which build the box carries.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use user::files::{self, Kind};

use super::trace;

/// The file system as `fwload` reaches it.
struct Files;

impl fwload::Reader for Files {
    fn size(&mut self, path: &str) -> Result<u64, i64> {
        match files::stat(path)? {
            (_, Kind::Dir) => Err(21), // EISDIR
            (size, Kind::File) => Ok(size),
        }
    }

    fn read(&mut self, path: &str, max: usize, out: &mut Vec<u8>) -> Result<(), i64> {
        files::read_large(path, max, out)
    }
}

/// Load the RTL8822C's firmware, or the reason it could not be.
pub(super) fn load() -> Result<Vec<u8>, fwload::Error> {
    fwload::load(&mut Files, rtw88::FIRMWARE, fwload::DEFAULT_MAX_BYTES)
}

/// The version fields of a firmware header (upstream `struct rtw_fw_hdr`).
pub(super) struct Header {
    pub(super) signature: u16,
    pub(super) version: u16,
    pub(super) sub_version: u8,
    pub(super) sub_index: u8,
    pub(super) h2c_version: u16,
    /// `YYYY-MM-DD hh:mm` of the build, as the header records it.
    pub(super) built: String,
    pub(super) dmem_size: u32,
    pub(super) imem_size: u32,
    pub(super) emem_size: u32,
}

/// Upstream's header size (`FW_HDR_SIZE`).
const HEADER_BYTES: usize = 64;

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// The header of `image`, if it is long enough to have one.
pub(super) fn header(image: &[u8]) -> Option<Header> {
    let h = image.get(..HEADER_BYTES)?;
    Some(Header {
        signature: le16(h, 0x00),
        version: le16(h, 0x04),
        sub_version: h[0x06],
        sub_index: h[0x07],
        h2c_version: le16(h, 0x1C),
        built: format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            le16(h, 0x14),
            h[0x10],
            h[0x11],
            h[0x12],
            h[0x13]
        ),
        dmem_size: le32(h, 0x24),
        imem_size: le32(h, 0x30),
        emem_size: le32(h, 0x34),
    })
}

/// `WIFID:FW:FILE name=... bytes=... signature=0x8822 version=9.9.15 ...`.
pub(super) fn report(image: &[u8]) {
    let name = rtw88::FIRMWARE;
    let bytes = image.len();
    let Some(h) = header(image) else {
        trace::event("FW:FILE", &[("name", &name), ("bytes", &bytes), ("header", &"short")]);
        return;
    };
    let signature = format!("{:#06x}", h.signature);
    let version = format!("{}.{}.{}", h.version, h.sub_version, h.sub_index);
    trace::event(
        "FW:FILE",
        &[
            ("name", &name),
            ("bytes", &bytes),
            ("signature", &signature),
            ("version", &version),
            ("h2c", &h.h2c_version),
            ("built", &h.built),
            ("dmem", &h.dmem_size),
            ("imem", &h.imem_size),
            ("emem", &h.emem_size),
        ],
    );
}
