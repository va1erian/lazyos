//! Sustained load: millions of events, several producers, mixed drain
//! cadences. The invariant throughout is the bus contract: **no loss without a
//! marker** and a gapless sequence.

use super::*;
use crate::input::raw_tap::Tap;

/// A small deterministic generator (xorshift64*), so failures reproduce.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// Running verifier for one consumer's stream.
struct Watch {
    expected: u64,
    delivered: u64,
    lost: u64,
    markers: u64,
}

impl Watch {
    fn new() -> Self {
        Watch {
            expected: 1,
            delivered: 0,
            lost: 0,
            markers: 0,
        }
    }

    fn take(&mut self, records: &[RawEvent]) -> Result<(), String> {
        let (next, lost) = gapless(records, self.expected)?;
        self.expected = next;
        self.lost += lost;
        let markers = records.iter().filter(|r| r.kind == kind::DROPPED).count() as u64;
        self.markers += markers;
        self.delivered += records.len() as u64 - markers;
        Ok(())
    }
}

/// Two million events, bursts and drains of random size: every event is
/// either delivered or covered by a `Dropped` marker, in both regimes (bursts
/// that fit and bursts that overrun).
pub fn no_silent_loss() -> Result<(), String> {
    const TOTAL: u64 = 2_000_000;
    fresh();
    let owner = scratch()?;
    bus::open(owner).map_err(|e| format!("{e:?}"))?;
    let id = bus::consumer_of(owner).ok_or("no consumer")?;
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut watch = Watch::new();
    let (mut published, mut clean_batches) = (0u64, 0u64);
    let mut out = Vec::with_capacity(RING_CAP + 1);
    while published < TOTAL {
        // Mostly bursts a ring holds, sometimes one that overruns it.
        let burst = if rng.below(8) == 0 {
            RING_CAP as u64 + 1 + rng.below(600)
        } else {
            1 + rng.below(RING_CAP as u64 - 1)
        };
        for _ in 0..burst.min(TOTAL - published) {
            bus::publish(
                bus::device::PS2_KEYBOARD,
                kind::KEY,
                (published & 0xFF) as u16,
                (published & 1) as i32,
            );
            published += 1;
        }
        // Drain in a few random-sized reads; sometimes leave data behind.
        for _ in 0..(1 + rng.below(3)) {
            out.clear();
            bus::drain(id, owner, 1 + rng.below(300) as usize, &mut out)
                .map_err(|e| format!("{e:?}"))?;
            let markers = out.iter().filter(|r| r.kind == kind::DROPPED).count();
            clean_batches += (markers == 0 && !out.is_empty()) as u64;
            watch.take(&out)?;
        }
    }
    watch.take(&drain_all(owner, 97)?)?;
    check!(
        watch.expected == TOTAL + 1,
        "sequence ended at {}, want {}",
        watch.expected,
        TOTAL + 1
    );
    check!(
        watch.delivered + watch.lost == TOTAL,
        "delivered {} + lost {} != {TOTAL}",
        watch.delivered,
        watch.lost
    );
    check!(
        watch.markers > 0 && watch.lost > 0,
        "no overrun was exercised"
    );
    check!(clean_batches > 100, "no lossless regime was exercised");
    bus::reset();
    Ok(())
}

/// Three interleaved "devices" feed two consumers with different appetites;
/// each consumer sees a gapless stream and each device's own order is kept.
pub fn many_producers() -> Result<(), String> {
    const TOTAL: u64 = 1_500_000;
    fresh();
    let (fast, slow) = (scratch()?, scratch()?);
    bus::open(fast).map_err(|e| format!("{e:?}"))?;
    bus::open(slow).map_err(|e| format!("{e:?}"))?;
    let (fast_id, slow_id) = (
        bus::consumer_of(fast).ok_or("no ring")?,
        bus::consumer_of(slow).ok_or("no ring")?,
    );
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let mut counters = [0i32; 3];
    let mut last_seen = [[0i32; 3]; 2];
    let mut watches = [Watch::new(), Watch::new()];
    let mut out = Vec::new();
    let mut published = 0u64;
    while published < TOTAL {
        for _ in 0..(1 + rng.below(400)).min(TOTAL - published) {
            let device = rng.below(3) as usize;
            counters[device] += 1;
            bus::publish(1 + device as u8, kind::KEY, device as u16, counters[device]);
            published += 1;
        }
        for (index, (id, owner, batch)) in [(fast_id, fast, 512usize), (slow_id, slow, 7)]
            .into_iter()
            .enumerate()
        {
            // The slow consumer reads only some rounds.
            if index == 1 && rng.below(3) != 0 {
                continue;
            }
            out.clear();
            bus::drain(id, owner, batch, &mut out).map_err(|e| format!("{e:?}"))?;
            for record in out.iter().filter(|r| r.kind == kind::KEY) {
                let device = record.code as usize;
                check!(
                    record.device == 1 + device as u8 && record.value > last_seen[index][device],
                    "consumer {index}: device {device} went {} -> {}",
                    last_seen[index][device],
                    record.value
                );
                last_seen[index][device] = record.value;
            }
            watches[index].take(&out)?;
        }
    }
    for (index, owner) in [fast, slow].into_iter().enumerate() {
        watches[index].take(&drain_all(owner, 64)?)?;
        check!(
            watches[index].expected == TOTAL + 1
                && watches[index].delivered + watches[index].lost == TOTAL,
            "consumer {index}: ended at {}, delivered {} lost {}",
            watches[index].expected,
            watches[index].delivered,
            watches[index].lost
        );
    }
    check!(watches[1].lost > 0, "the slow consumer never overran");
    bus::reset();
    Ok(())
}

/// A storm of arbitrary scancode bytes (valid sequences, prefixes, garbage)
/// through the driver tap: the decoder never wedges, every record is a
/// well-formed key edge, and the stream stays gapless.
pub fn scancode_storm() -> Result<(), String> {
    const BYTES: u64 = 600_000;
    fresh();
    let owner = scratch()?;
    bus::open(owner).map_err(|e| format!("{e:?}"))?;
    let mut rng = Rng(0xA076_1D64_78BD_642F);
    let mut tap = Tap::new();
    let mut watch = Watch::new();
    let mut fed = 0u64;
    let mut out = Vec::new();
    let id = bus::consumer_of(owner).ok_or("no ring")?;
    while fed < BYTES {
        for _ in 0..(1 + rng.below(300)) {
            // Half the bytes are prefixes and make/break of real keys, so
            // multi-byte sequences actually complete.
            let byte = match rng.below(8) {
                0 => 0xE0,
                1 => 0xE1,
                2 => 0xFF,
                _ => rng.below(256) as u8,
            };
            tap.feed(byte);
            fed += 1;
        }
        out.clear();
        bus::drain(id, owner, RING_CAP + 1, &mut out).map_err(|e| format!("{e:?}"))?;
        for record in out.iter().filter(|r| r.kind != kind::DROPPED) {
            check!(
                record.kind == kind::KEY
                    && (0x04..=0xE7).contains(&record.code)
                    && (record.value == 0 || record.value == 1)
                    && record.device == bus::device::PS2_KEYBOARD,
                "malformed record {record:?}"
            );
        }
        watch.take(&out)?;
    }
    check!(
        watch.delivered > 10_000,
        "storm produced {} keys",
        watch.delivered
    );
    check!(
        crate::input::raw_tap::unknown_count() > 0,
        "garbage was not counted"
    );
    bus::reset();
    Ok(())
}
