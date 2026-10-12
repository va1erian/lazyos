//! The `diag.dbg.mmio*` lines of `/boot/lazyos.cfg`.
//!
//! ```text
//! diag.dbg=1                              # dbgd on (everything below needs it)
//! diag.dbg.mmio=1                         # MMIO reads, inside allowed ranges
//! diag.dbg.mmio.claimed=1                 # ...also of devices a driver holds
//! diag.dbg.mmio.allow=8086:5916:0:0x0-0x1000   # one more range (vendor:device:bar:start-end)
//! ```
//!
//! The file sits on the unencrypted boot volume next to the key, so whoever
//! can write it can already do anything `dbgd` can: a range line is the box
//! owner's decision, not the client's. Numbers are hex (`0x` optional).

use alloc::vec::Vec;

use crate::{Policy, Range, MAX_EXTRA};

pub const MMIO_KEY: &str = "diag.dbg.mmio";
pub const CLAIMED_KEY: &str = "diag.dbg.mmio.claimed";
pub const ALLOW_KEY: &str = "diag.dbg.mmio.allow";

fn hex(text: &str) -> Option<u64> {
    let text = text.trim();
    let digits = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    if digits.is_empty() || digits.len() > 16 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(digits, 16).ok()
}

/// `VVVV:DDDD:BAR:START-END`, hex; `None` for anything that is not a valid
/// [`Range`].
pub fn parse_range(text: &str) -> Option<Range> {
    let mut parts = text.trim().split(':');
    let vendor = u16::try_from(hex(parts.next()?)?).ok()?;
    let device = u16::try_from(hex(parts.next()?)?).ok()?;
    let bar = u8::try_from(hex(parts.next()?)?).ok()?;
    let (start, end) = parts.next()?.split_once('-')?;
    if parts.next().is_some() {
        return None;
    }
    let range = Range {
        vendor,
        device,
        bar,
        start: hex(start)?,
        end: hex(end)?,
    };
    range.valid().then_some(range)
}

pub(crate) fn parse(cfg: &str) -> Policy {
    let (mut enabled, mut mmio, mut claimed) = (false, false, false);
    let mut extra: Vec<Range> = Vec::new();
    let mut rejected = 0;
    for line in cfg.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "diag.dbg" => enabled = value == "1",
            MMIO_KEY => mmio = value == "1",
            CLAIMED_KEY => claimed = value == "1",
            ALLOW_KEY => match parse_range(value) {
                Some(range) if extra.len() < MAX_EXTRA => extra.push(range),
                _ => rejected += 1,
            },
            _ => {}
        }
    }
    if !(enabled && mmio) {
        return Policy::off();
    }
    Policy::with(true, claimed, extra, rejected)
}
