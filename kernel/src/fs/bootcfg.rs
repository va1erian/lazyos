//! `lazyos.cfg`: which volumes become `/` and `/home` (docs/filesystem-plan.md
//! F1).
//!
//! The file sits on the FAT boot volume and is read before anything is
//! mounted, so it is untrusted input: capped at [`MAX_BYTES`], UTF-8 only, and
//! rejected whole on any malformed or duplicate line rather than half applied.
//! [`parse`] is pure (no I/O) so the test suite can feed it hostile text.
//!
//! ```text
//! # comment
//! root=UUID=0b0c8d3a-5b1e-4a53-9d77-0123456789ab
//! home=LABEL=home          # or home=UUID=...
//! root_flags=noexec        # comma list of ro, noexec, nosuid
//! limit.heap_max=512M      # kernel limits: see `crate::limits`
//! display.mode=2560x1440   # display mode and scale: see `crate::display::modecfg`
//! ```
//!
//! `limit.*` lines belong to [`crate::limits`] and `display.*` lines to
//! [`crate::display::modeset`], which log each one on its own: they are
//! skipped here, so a bad limit or mode can never cost the boot its root
//! volume.

use alloc::string::String;
use alloc::vec::Vec;

use super::vfs::{Filesystem, MountFlags};

/// Prefix of the lines `xuid` reads itself (`diag.hold=<seconds>`, the boot
/// log hold of docs/compat/kabylake/B0.md); the kernel skips them.
const DIAG_PREFIX: &str = "diag.";

/// File name on the FAT volume, and the largest accepted size.
pub const FILE_NAME: &str = "lazyos.cfg";
pub const MAX_BYTES: usize = 4096;

/// How a volume is named: by superblock UUID or by (NUL-padded) label.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VolumeId {
    Uuid([u8; 16]),
    Label([u8; 16]),
}

/// A parsed config. Absent keys leave the defaults.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct BootCfg {
    pub root: Option<[u8; 16]>,
    pub home: Option<VolumeId>,
    pub root_flags: MountFlags,
    pub home_flags: MountFlags,
}

/// Why a config was refused.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CfgError {
    TooLarge,
    NotUtf8,
    Nul,
    /// A line with no `=`.
    Malformed(usize),
    Duplicate(String),
    BadValue(String),
}

impl core::fmt::Display for CfgError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CfgError::TooLarge => write!(f, "larger than {MAX_BYTES} bytes"),
            CfgError::NotUtf8 => write!(f, "not UTF-8"),
            CfgError::Nul => write!(f, "contains a NUL byte"),
            CfgError::Malformed(line) => write!(f, "line {line} is not key=value"),
            CfgError::Duplicate(key) => write!(f, "duplicate key {key}"),
            CfgError::BadValue(key) => write!(f, "bad value for {key}"),
        }
    }
}

/// Decode the raw file bytes, then [`parse`] them.
pub fn parse_bytes(bytes: &[u8]) -> Result<BootCfg, CfgError> {
    if bytes.len() > MAX_BYTES {
        return Err(CfgError::TooLarge);
    }
    parse(core::str::from_utf8(bytes).map_err(|_| CfgError::NotUtf8)?)
}

/// Parse config text. Unknown keys are logged and ignored.
pub fn parse(text: &str) -> Result<BootCfg, CfgError> {
    if text.len() > MAX_BYTES {
        return Err(CfgError::TooLarge);
    }
    if text.contains('\0') {
        return Err(CfgError::Nul);
    }
    let mut cfg = BootCfg::default();
    let mut seen: Vec<&str> = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with(crate::limits::PREFIX)
            || line.starts_with(DIAG_PREFIX)
            || line.starts_with(crate::display::modecfg::PREFIX)
        {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or(CfgError::Malformed(number + 1))?;
        let (key, value) = (key.trim(), value.trim());
        if seen.contains(&key) {
            return Err(CfgError::Duplicate(key.into()));
        }
        seen.push(key);
        let bad = || CfgError::BadValue(key.into());
        match key {
            "root" => cfg.root = Some(uuid_value(value).ok_or_else(bad)?),
            "home" => cfg.home = Some(volume_value(value).ok_or_else(bad)?),
            "root_flags" => cfg.root_flags = MountFlags::parse_list(value).map_err(|_| bad())?,
            "home_flags" => cfg.home_flags = MountFlags::parse_list(value).map_err(|_| bad())?,
            _ => serial_println!("fs: lazyos.cfg: unknown key {key} ignored"),
        }
    }
    Ok(cfg)
}

/// `UUID=<uuid>` -> the 16 bytes.
fn uuid_value(value: &str) -> Option<[u8; 16]> {
    parse_uuid(value.strip_prefix("UUID=")?)
}

/// `UUID=<uuid>` or `LABEL=<label>` (1 to 16 bytes, no control characters).
fn volume_value(value: &str) -> Option<VolumeId> {
    if let Some(label) = value.strip_prefix("LABEL=") {
        let raw = label.as_bytes();
        if raw.is_empty() || raw.len() > 16 || raw.iter().any(|b| b.is_ascii_control()) {
            return None;
        }
        let mut padded = [0u8; 16];
        padded[..raw.len()].copy_from_slice(raw);
        return Some(VolumeId::Label(padded));
    }
    uuid_value(value).map(VolumeId::Uuid)
}

/// The 36-character `8-4-4-4-12` form, in the byte order mkfs stores it.
pub fn parse_uuid(text: &str) -> Option<[u8; 16]> {
    let raw = text.as_bytes();
    if raw.len() != 36 {
        return None;
    }
    let mut out = [0u8; 16];
    let mut nibbles = raw
        .iter()
        .enumerate()
        .filter_map(|(at, &byte)| match (at, byte) {
            (8 | 13 | 18 | 23, b'-') => None,
            (8 | 13 | 18 | 23, _) => Some(None),
            _ => Some((byte as char).to_digit(16).map(|d| d as u8)),
        });
    for byte in out.iter_mut() {
        let high = nibbles.next()??;
        let low = nibbles.next()??;
        *byte = high << 4 | low;
    }
    Some(out)
}

/// Read and parse `lazyos.cfg` from the boot volume's backend. `None` when the
/// file is absent (silently) or unusable (logged as `fs: lazyos.cfg ignored`).
pub fn load(boot: &dyn Filesystem) -> Option<BootCfg> {
    let size = boot.stat(FILE_NAME).ok()?.size;
    let result = if size > MAX_BYTES as u64 {
        Err(CfgError::TooLarge)
    } else {
        let mut buf = alloc::vec![0u8; size as usize];
        match boot.read(FILE_NAME, 0, &mut buf) {
            Ok(read) => {
                if let Ok(text) = core::str::from_utf8(&buf[..read]) {
                    // The mode first: a switch re-derives the screen-sized
                    // limits, which the `limit.*` lines then override.
                    crate::display::modeset::apply_config(text);
                    crate::limits::apply_config(text);
                }
                parse_bytes(&buf[..read])
            }
            Err(_) => return None,
        }
    };
    match result {
        Ok(cfg) => Some(cfg),
        Err(reason) => {
            serial_println!("fs: lazyos.cfg ignored: {reason}");
            None
        }
    }
}
