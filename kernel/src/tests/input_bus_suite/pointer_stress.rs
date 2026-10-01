//! Sustained mixed load: keys and pointer records from several producers,
//! with tail merging live. The invariants are the bus contract (gapless,
//! identical streams, no loss without a marker) plus conservation: a consumer
//! that never overruns receives every motion count, notch and edge.

use super::*;
use crate::input::bus::{device, pointer};

/// Deterministic xorshift64*, so failures reproduce.
struct Rng(u64);

impl Rng {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) % bound
    }

    fn delta(&mut self) -> i16 {
        self.below(101) as i16 - 50
    }
}

/// What a lossless consumer must add up to.
#[derive(Default, PartialEq, Debug)]
struct Totals {
    dx: i64,
    dy: i64,
    scroll: i64,
    buttons: u64,
    keys: u64,
}

impl Totals {
    fn take(&mut self, record: &RawEvent) {
        match record.kind {
            kind::REL_MOTION => {
                let (dx, dy) = pointer::unpack_rel(record.value);
                self.dx += i64::from(dx);
                self.dy += i64::from(dy);
            }
            kind::SCROLL => self.scroll += i64::from(record.value),
            kind::BUTTON => self.buttons += 1,
            kind::KEY => self.keys += 1,
            _ => {}
        }
    }
}

/// Publish one random record, accounting for it in `sent`. Bursts are short
/// enough (see the caller) that merged deltas never saturate.
fn publish_one(rng: &mut Rng, sent: &mut Totals) {
    match rng.below(10) {
        0..=4 => {
            let (dx, dy) = (rng.delta(), rng.delta());
            let source = if rng.below(4) == 0 {
                3
            } else {
                device::PS2_MOUSE
            };
            bus::publish(source, kind::REL_MOTION, 0, pointer::pack_rel(dx, dy));
            sent.dx += i64::from(dx);
            sent.dy += i64::from(dy);
        }
        5 => {
            let notches = rng.below(5) as i32 - 2;
            bus::publish(device::PS2_MOUSE, kind::SCROLL, pointer::VERTICAL, notches);
            sent.scroll += i64::from(notches);
        }
        6 => {
            let x = rng.below(0x1_0000) as u16;
            bus::publish(3, kind::ABS_MOTION, 0, pointer::pack_abs(x, !x));
        }
        7 => {
            let usage = 1 + rng.below(5) as u16;
            bus::publish(device::PS2_MOUSE, kind::BUTTON, usage, rng.below(2) as i32);
            sent.buttons += 1;
        }
        _ => {
            bus::publish(device::PS2_KEYBOARD, kind::KEY, 0x04, rng.below(2) as i32);
            sent.keys += 1;
        }
    }
}

/// Three million records, a fast consumer that never overruns and a slow one
/// that does. The fast one must account for every count; both must be gapless
/// with monotonic timestamps; merging must actually have happened.
pub fn mixed_producers() -> Result<(), String> {
    const TOTAL: u64 = 3_000_000;
    fresh();
    let (fast, slow) = (scratch()?, scratch()?);
    bus::open(fast).map_err(|e| format!("{e:?}"))?;
    bus::open(slow).map_err(|e| format!("{e:?}"))?;
    let fast_id = bus::consumer_of(fast).ok_or("no ring")?;
    let slow_id = bus::consumer_of(slow).ok_or("no ring")?;
    let mut rng = Rng(0x5851_F42D_4C95_7F2D);
    let (mut sent, mut got) = (Totals::default(), Totals::default());
    let (mut fast_seq, mut slow_seq, mut slow_lost) = (1u64, 1u64, 0u64);
    let (mut published, mut delivered, mut last_ts) = (0u64, 0u64, 0u64);
    let mut out = Vec::with_capacity(RING_CAP + 1);
    while published < TOTAL {
        // Under a ring's worth, so the fast consumer never loses anything.
        for _ in 0..(1 + rng.below(RING_CAP as u64 - 1)) {
            publish_one(&mut rng, &mut sent);
            published += 1;
        }
        out.clear();
        bus::drain(fast_id, fast, RING_CAP, &mut out).map_err(|e| format!("{e:?}"))?;
        check!(
            out.iter().all(|r| r.kind != kind::DROPPED),
            "fast consumer overran"
        );
        for record in &out {
            check!(record.ts_ns >= last_ts, "timestamp went backwards");
            last_ts = record.ts_ns;
            got.take(record);
        }
        delivered += out.len() as u64;
        fast_seq = gapless(&out, fast_seq)?.0;
        if rng.below(4) == 0 {
            out.clear();
            bus::drain(slow_id, slow, 7, &mut out).map_err(|e| format!("{e:?}"))?;
            let (next, lost) = gapless(&out, slow_seq)?;
            slow_seq = next;
            slow_lost += lost;
        }
    }
    let rest = drain_all(slow, 64)?;
    let (slow_end, lost) = gapless(&rest, slow_seq)?;
    slow_lost += lost;
    check!(got == sent, "fast consumer got {got:?}, sent {sent:?}");
    check!(
        fast_seq == slow_end,
        "consumers disagree on the end: {fast_seq} vs {slow_end}"
    );
    check!(
        delivered < published,
        "nothing merged ({delivered} of {published})"
    );
    check!(slow_lost > 0, "the slow consumer never overran");
    bus::reset();
    Ok(())
}
