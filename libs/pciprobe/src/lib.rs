//! What `dbgd` may read of a PCI device without owning it: configuration
//! space, and memory BARs through an allowlist (docs/dbgd-plan.md, "Hardware
//! reads"). Shared by the kernel, which enforces it, and by `dbgwire` and
//! `dbgd`, which use the same rules to refuse early and explain.
//!
//! Pure `no_std` + `alloc`, host-tested and fuzzed, because the request and
//! the `lazyos.cfg` lines both come from outside.
//!
//! * Configuration space is read only when `diag.dbg=1` (as the kernel read
//!   it), any alignment, never a write. The standard 64-byte header
//!   ([`HEADER_BYTES`]) is the default; the rest of the legacy 256 bytes
//!   ([`CONFIG_BYTES`]; the kernel has no MMCONFIG yet) needs
//!   `diag.dbg.mmio=1` too, because some devices misbehave when their
//!   device-specific registers are read (why `lspci` limits the full dump
//!   to root).
//! * An MMIO read is answered only when `diag.dbg.mmio=1`, only for 32-bit
//!   aligned dwords, at most [`MAX_MMIO_BYTES`] per request, and only
//!   inside ranges listed for that exact `vendor:device` and BAR
//!   ([`builtin::RANGES`], plus `diag.dbg.mmio.allow=` lines). A register
//!   is not allowed because it is not known to be dangerous: it is allowed
//!   because somebody listed it.
//! * A device a driver has claimed is refused unless
//!   `diag.dbg.mmio.claimed=1` ([`Policy::claimed`]): a read racing the
//!   driver can hit a register the driver is in the middle of sequencing.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod builtin;
mod cfg;
pub mod fuzz;

use alloc::string::String;
use alloc::vec::Vec;

pub use cfg::{parse_range, ALLOW_KEY, CLAIMED_KEY, MMIO_KEY};

/// Bytes of configuration space `dbgd` can read at most (mechanism 1's 256).
pub const CONFIG_BYTES: u64 = 256;
/// The standard header, readable without the MMIO opt-in.
pub const HEADER_BYTES: u64 = 64;
/// Most bytes one MMIO read returns.
pub const MAX_MMIO_BYTES: u64 = 4096;
/// Highest BAR index.
pub const MAX_BAR: u8 = 5;
/// `diag.dbg.mmio.allow=` lines taken from a config (the file is 4 KiB).
pub const MAX_EXTRA: usize = 8;
/// Largest range a config line may add: the kernel maps whole ranges.
pub const MAX_RANGE_BYTES: u64 = 64 << 20;

/// One readable window of one BAR of one `vendor:device`, `end` exclusive.
/// Offsets and length are multiples of 4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub vendor: u16,
    pub device: u16,
    pub bar: u8,
    pub start: u64,
    pub end: u64,
}

impl Range {
    /// Whether the range is well formed: ordered, dword aligned, bounded.
    pub fn valid(&self) -> bool {
        self.bar <= MAX_BAR
            && self.start < self.end
            && self.start.is_multiple_of(4)
            && self.end.is_multiple_of(4)
            && self.end - self.start <= MAX_RANGE_BYTES
    }
}

/// Why a read was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denied {
    /// Hardware reads are not switched on at all (`diag.dbg=1`).
    Disabled,
    /// The MMIO tier is not switched on (`diag.dbg.mmio=1`).
    Off,
    /// Config bytes 0x40..0x100 need `diag.dbg.mmio=1`.
    ExtendedConfig,
    /// Zero length, more than the maximum, or the span wraps.
    Length,
    /// Offset or length not a multiple of 4.
    Alignment,
    /// BAR index above 5.
    Bar,
    /// Some byte of the span is in no allowed range.
    NotAllowed,
}

impl Denied {
    pub fn text(self) -> &'static str {
        match self {
            Denied::Disabled => "hardware reads are off on this box (diag.dbg=1 in lazyos.cfg)",
            Denied::Off => "MMIO reads are off on this box (diag.dbg.mmio=1 in lazyos.cfg)",
            Denied::ExtendedConfig => {
                "config space past the 64-byte header needs diag.dbg.mmio=1 in lazyos.cfg"
            }
            Denied::Length => "length must be 1..=256 for config space, 4..=4096 for MMIO",
            Denied::Alignment => "MMIO reads are 32-bit: offset and length are multiples of 4",
            Denied::Bar => "BAR index is 0..=5",
            Denied::NotAllowed => "the span is not inside a range allowed for this device",
        }
    }
}

/// A run of one request that lies inside one allowed range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub range: Range,
    /// Offset in the BAR of the run's first byte.
    pub offset: u64,
    /// Bytes in the run.
    pub len: u64,
}

/// The box's settings and the extra ranges its config adds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// `diag.dbg=1`: hardware reads (config header) may be answered.
    pub enabled: bool,
    /// MMIO reads on at all (`diag.dbg=1` and `diag.dbg.mmio=1`).
    pub mmio: bool,
    /// Reads of a device a driver holds (`diag.dbg.mmio.claimed=1`).
    pub claimed: bool,
    extra: Vec<Range>,
    /// Config lines that named a range and were refused (malformed, over
    /// [`MAX_EXTRA`]); the kernel logs the count.
    pub rejected: usize,
}

impl Policy {
    /// The policy of a box that did not ask: nothing but config space.
    pub const fn off() -> Policy {
        Policy {
            enabled: false,
            mmio: false,
            claimed: false,
            extra: Vec::new(),
            rejected: 0,
        }
    }

    /// Read the `diag.dbg.mmio*` lines of `cfg`. `diag.dbg=1` must hold too
    /// (a later duplicate wins, as for every `diag.dbg` key).
    pub fn from_cfg(cfg: &str) -> Policy {
        cfg::parse(cfg)
    }

    pub(crate) fn with(mmio: bool, claimed: bool, extra: Vec<Range>, rejected: usize) -> Policy {
        Policy {
            enabled: true,
            mmio,
            claimed,
            extra,
            rejected,
        }
    }

    /// A box with `diag.dbg=1` and no MMIO opt-in: the config header only.
    pub fn config_only() -> Policy {
        Policy::with(false, false, Vec::new(), 0)
    }

    /// Check a configuration-space read of `len` bytes at `offset`.
    pub fn check_config(&self, offset: u64, len: u64) -> Result<(), Denied> {
        if !self.enabled {
            return Err(Denied::Disabled);
        }
        let end = match offset.checked_add(len) {
            Some(end) if len >= 1 && end <= CONFIG_BYTES => end,
            _ => return Err(Denied::Length),
        };
        if end > HEADER_BYTES && !self.mmio {
            return Err(Denied::ExtendedConfig);
        }
        Ok(())
    }

    /// A policy with MMIO on and these extra ranges (tests, harnesses).
    pub fn with_ranges(claimed: bool, extra: &[Range]) -> Policy {
        let valid: Vec<Range> = extra.iter().filter(|r| r.valid()).copied().collect();
        Policy::with(true, claimed, valid, 0)
    }

    /// The ranges a config added.
    pub fn extra(&self) -> &[Range] {
        &self.extra
    }

    /// Every range allowed for `vendor:device` BAR `bar`, sorted by start:
    /// the built-in ones and the config's.
    pub fn ranges(&self, vendor: u16, device: u16, bar: u8) -> Vec<Range> {
        let mut found: Vec<Range> = builtin::RANGES
            .iter()
            .chain(self.extra.iter())
            .filter(|r| r.vendor == vendor && r.device == device && r.bar == bar)
            .copied()
            .collect();
        found.sort_by_key(|r| (r.start, r.end));
        found
    }

    /// Cut the request `[offset, offset + len)` of BAR `bar` of
    /// `vendor:device` into runs, each inside one allowed range, or say why
    /// not. Adjacent and overlapping allowed ranges join: a request may
    /// span them, and every byte must be covered.
    pub fn plan(
        &self,
        vendor: u16,
        device: u16,
        bar: u8,
        offset: u64,
        len: u64,
    ) -> Result<Vec<Segment>, Denied> {
        if !self.enabled {
            return Err(Denied::Disabled);
        }
        if !self.mmio {
            return Err(Denied::Off);
        }
        if bar > MAX_BAR {
            return Err(Denied::Bar);
        }
        if len == 0 || len > MAX_MMIO_BYTES {
            return Err(Denied::Length);
        }
        if !offset.is_multiple_of(4) || !len.is_multiple_of(4) {
            return Err(Denied::Alignment);
        }
        let end = offset.checked_add(len).ok_or(Denied::Length)?;
        let ranges = self.ranges(vendor, device, bar);
        let mut out = Vec::new();
        let mut at = offset;
        while at < end {
            // The covering range that reaches furthest.
            let best = ranges
                .iter()
                .filter(|r| r.start <= at && at < r.end)
                .max_by_key(|r| r.end)
                .ok_or(Denied::NotAllowed)?;
            let stop = best.end.min(end);
            out.push(Segment {
                range: *best,
                offset: at,
                len: stop - at,
            });
            at = stop;
        }
        Ok(out)
    }
}

/// `bus:device.function` (hex, as `lspci` writes it) as the 16-bit
/// `bus << 8 | device << 3 | function`. A leading `0000:` domain is
/// accepted and must be zero.
pub fn parse_bdf(text: &str) -> Option<u16> {
    let text = text.trim();
    let text = match text.split_once(':') {
        Some((domain, rest)) if rest.contains(':') => {
            (domain.len() == 4 && u16::from_str_radix(domain, 16).ok()? == 0).then_some(rest)?
        }
        _ => text,
    };
    let (bus, rest) = text.split_once(':')?;
    let (device, function) = rest.split_once('.')?;
    let hex = |s: &str, max: usize| {
        (!s.is_empty() && s.len() <= max && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| u8::from_str_radix(s, 16).ok())
            .flatten()
    };
    let (bus, device, function) = (hex(bus, 2)?, hex(device, 2)?, hex(function, 1)?);
    (device < 32 && function < 8)
        .then_some(u16::from(bus) << 8 | u16::from(device) << 3 | u16::from(function))
}

/// The `bb:dd.f` spelling of a BDF.
pub fn format_bdf(bdf: u16) -> String {
    alloc::format!("{:02x}:{:02x}.{}", bdf >> 8, (bdf >> 3) & 0x1F, bdf & 7)
}

#[cfg(test)]
mod tests;
