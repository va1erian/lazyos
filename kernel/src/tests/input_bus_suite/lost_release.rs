//! A lost PS/2 release is repaired by the next press (issue #400): a make for
//! a key the tap thinks is down that arrives too late to be typematic becomes
//! a release + press pair on the bus.

use super::*;
use crate::input::raw_tap::{self, Tap, STALE_NS};

/// Set-1 make and break of `A` (HID 0x04) and of Left Shift (HID 0xE1).
const A_MAKE: u8 = 0x1E;
const A_BREAK: u8 = 0x9E;
const SHIFT_MAKE: u8 = 0x2A;
const SHIFT_BREAK: u8 = 0xAA;

fn edges(records: &[RawEvent]) -> Vec<(u16, i32)> {
    records.iter().map(|r| (r.code, r.value)).collect()
}

/// Typematic gaps stay suppressed; a stale re-press publishes the missing
/// release before the press, for plain and prefixed keys alike.
pub fn stale_repress_releases_first() -> Result<(), String> {
    fresh();
    let owner = scratch()?;
    bus::open(owner).map_err(|e| format!("{e:?}"))?;
    let mut tap = Tap::new();
    let resynced = raw_tap::resynced_presses();
    let second = 1_000_000_000;
    // Shift held with typematic every 0.5 s: three repeats, all swallowed,
    // even though the whole hold lasts longer than the stale gap.
    tap.feed_at(SHIFT_MAKE, second);
    for step in 1..=3 {
        tap.feed_at(SHIFT_MAKE, second + step * STALE_NS / 3);
    }
    // Its break is lost; a press 2 s later is a new press.
    tap.feed_at(SHIFT_MAKE, second + STALE_NS + 2 * second);
    tap.feed_at(SHIFT_BREAK, second + STALE_NS + 2 * second + 1);
    // Right arrow (E0 4D -> HID 0x4F): lost break, late re-press.
    tap.feed_at(0xE0, 10 * second);
    tap.feed_at(0x4D, 10 * second);
    tap.feed_at(0xE0, 10 * second + STALE_NS);
    tap.feed_at(0x4D, 10 * second + STALE_NS);
    tap.feed_at(0xE0, 10 * second + STALE_NS + 1);
    tap.feed_at(0xCD, 10 * second + STALE_NS + 1);
    let got = edges(&drain_all(owner, 32)?);
    let want = [
        (0xE1, 1),
        (0xE1, 0),
        (0xE1, 1),
        (0xE1, 0),
        (0x4F, 1),
        (0x4F, 0),
        (0x4F, 1),
        (0x4F, 0),
    ];
    check!(got == want, "edges {got:x?}");
    check!(
        raw_tap::resynced_presses() - resynced == 2,
        "resync counter"
    );
    bus::reset();
    Ok(())
}

/// A gap just under the limit is still typematic; a normal release resets
/// the clock so a quick press after it is an ordinary press.
pub fn boundary_and_normal_release() -> Result<(), String> {
    fresh();
    let owner = scratch()?;
    bus::open(owner).map_err(|e| format!("{e:?}"))?;
    let mut tap = Tap::new();
    let start = 5_000_000_000;
    tap.feed_at(A_MAKE, start);
    tap.feed_at(A_MAKE, start + STALE_NS - 1);
    tap.feed_at(A_BREAK, start + STALE_NS);
    tap.feed_at(A_MAKE, start + STALE_NS + 10);
    tap.feed_at(A_BREAK, start + STALE_NS + 20);
    let got = edges(&drain_all(owner, 32)?);
    check!(
        got == [(0x04, 1), (0x04, 0), (0x04, 1), (0x04, 0)],
        "edges {got:x?}"
    );
    bus::reset();
    Ok(())
}

/// Stress: many keys, long holds with typematic, randomly lost releases and
/// late re-presses. The bus must stay balanced: every press is followed by
/// exactly one release of that key before its next press, and nothing is
/// held once every key is finally released.
pub fn soak_lost_releases_balance() -> Result<(), String> {
    fresh();
    let owner = scratch()?;
    bus::open(owner).map_err(|e| format!("{e:?}"))?;
    let id = bus::consumer_of(owner).ok_or("no consumer")?;
    let mut tap = Tap::new();
    let mut held = [false; 256];
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut now = 1_000_000_000u64;
    let mut out = Vec::new();
    let mut checked = 0usize;
    // Set-1 makes of the letters q..p, a..l, z..m (HID 0x04..0x1D range).
    let keys: [u8; 26] = [
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1E, 0x1F, 0x20, 0x21, 0x22,
        0x23, 0x24, 0x25, 0x26, 0x2C, 0x2D, 0x2E, 0x2F, 0x30, 0x31, 0x32,
    ];
    for _ in 0..20_000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let make = keys[(seed % keys.len() as u64) as usize];
        now += match seed >> 60 {
            0..=9 => 30_000_000,    // fast typing / typematic
            10..=13 => 600_000_000, // a pause
            _ => 2 * STALE_NS,      // long enough to make a re-press stale
        };
        tap.feed_at(make, now);
        // Usually release it; sometimes "lose" the break.
        if (seed >> 8) % 5 != 0 {
            now += 10_000_000;
            tap.feed_at(make | 0x80, now);
        }
        bus::drain(id, owner, RING_CAP, &mut out).map_err(|e| format!("{e:?}"))?;
        for record in out.drain(..) {
            check!(record.kind == kind::KEY, "unexpected kind {}", record.kind);
            let code = record.code as usize;
            match record.value {
                1 => check!(!held[code], "double press of {code:#x}"),
                0 => check!(held[code], "release of idle {code:#x}"),
                other => return Err(format!("value {other}")),
            }
            held[code] = record.value == 1;
            checked += 1;
        }
    }
    // Release everything: nothing may stay held.
    for make in keys {
        now += 10_000_000;
        tap.feed_at(make | 0x80, now);
    }
    bus::drain(id, owner, RING_CAP, &mut out).map_err(|e| format!("{e:?}"))?;
    for record in out.drain(..) {
        held[record.code as usize] = record.value == 1;
    }
    check!(held.iter().all(|h| !h), "keys left held");
    check!(checked > 10_000, "only {checked} edges checked");
    bus::reset();
    Ok(())
}
