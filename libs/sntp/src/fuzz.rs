//! Byte-level fuzzing of the SNTP reply path and the schedule.
//!
//! [`run`] is the shared entry point for the cargo-fuzz target
//! (`fuzz/fuzz_targets/sntp.rs`) and the seeded tests below. The first byte
//! picks the half: even is a reply (`[sent:8][rtt:8][packet..]`, little
//! endian, the packet's own originate field is used as the nonce so the
//! later checks are reachable), odd is a script of schedule calls. Whatever
//! the bytes are, nothing may panic, and what is accepted must satisfy the
//! invariants below.

use core::num::NonZeroU64;

use crate::packet::{decode_reply, PACKET_LEN};
use crate::policy::{decide, Decision, CLOCK_FLOOR_UNIX, MAX_STEP_NS, MAX_UNIX_SECS, MIN_STEP_NS};
use crate::sample::{Sample, MAX_DELAY_NS, MAX_ROOT_DISTANCE_NS};
use crate::schedule::{Schedule, MIN_INTERVAL_MS};
use crate::NS_PER_SEC;

pub fn run(input: &[u8]) {
    match input.split_first() {
        Some((first, rest)) => match first & 3 {
            0 | 3 => reply(rest),
            1 => schedule(rest),
            _ => persistence(rest),
        },
        None => {}
    }
}

fn le64(bytes: &[u8]) -> u64 {
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(raw)
}

fn reply(input: &[u8]) {
    if input.len() < 16 {
        return;
    }
    let sent = le64(&input[0..8]) as i64;
    let rtt = le64(&input[8..16]);
    let packet = &input[16..];
    // The nonce the packet claims to answer, when it has one to claim.
    let claimed = if packet.len() >= 32 {
        be64(&packet[24..32])
    } else {
        1
    };
    let nonce = NonZeroU64::new(claimed).unwrap_or(NonZeroU64::MIN);
    // A different nonce never matches (unless the packet is too short or
    // malformed before that check).
    let wrong = NonZeroU64::new(nonce.get().wrapping_add(1)).unwrap_or(NonZeroU64::MIN);
    assert!(
        decode_reply(packet, wrong).is_err(),
        "a reply was accepted for a nonce it does not echo"
    );
    let Ok(reply) = decode_reply(packet, nonce) else {
        return;
    };
    assert!(packet.len() >= PACKET_LEN);
    assert!((1..=15).contains(&reply.stratum));
    assert!(matches!(reply.version, 3 | 4));
    assert!((reply.transmit.0.wrapping_sub(reply.receive.0) as i64) >= 0);
    assert!(!reply.receive.is_zero() && !reply.transmit.is_zero());

    let Ok(sample) = Sample::measure(&reply, sent, rtt) else {
        return;
    };
    assert!((0..=MAX_DELAY_NS).contains(&sample.delay_ns));
    assert!(sample.root_distance_ns <= MAX_ROOT_DISTANCE_NS);
    for first in [true, false] {
        match decide(sent, sample.offset_ns, first) {
            Decision::Keep => {
                assert!(i128::from(sample.offset_ns).abs() < i128::from(MIN_STEP_NS));
            }
            Decision::Step { target_unix_ns } => {
                let secs = target_unix_ns.div_euclid(NS_PER_SEC);
                assert!((CLOCK_FLOOR_UNIX..MAX_UNIX_SECS).contains(&secs));
                let size = i128::from(sample.offset_ns).abs();
                assert!(size >= i128::from(MIN_STEP_NS));
                assert!(first || size <= i128::from(MAX_STEP_NS));
            }
            Decision::Refuse(_) => {}
        }
    }
}

fn be64(bytes: &[u8]) -> u64 {
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&bytes[..8]);
    u64::from_be_bytes(raw)
}

/// Feed `Persistence` offsets and times from a script of 9-byte records
/// (`[ms delta: u8][offset: i64 LE]`): it never says yes before three
/// observations, never before ten minutes since the first of the run that
/// agrees, and never panics.
fn persistence(input: &[u8]) {
    use crate::policy::{Persistence, PERSIST_COUNT, PERSIST_MIN_MS};

    let mut p = Persistence::new();
    let (mut now, mut seen) = (0u64, 0u32);
    for record in input.chunks_exact(9) {
        now = now.saturating_add(u64::from(record[0]) * 5_000);
        let offset = i64::from_le_bytes(record[1..9].try_into().unwrap());
        seen += 1;
        if p.observe(now, offset) {
            assert!(seen >= PERSIST_COUNT, "believed after {seen} observations");
            assert!(now >= PERSIST_MIN_MS, "believed at {now} ms");
        }
        if record[0] == 0xff {
            p.reset();
            seen = 0;
        }
    }
}

/// Drive a schedule the way `timed` does (query only when due, record it)
/// from a script, and hold it to its promises.
fn schedule(input: &[u8]) {
    let mut s = Schedule::new();
    let mut now: u64 = 0;
    let mut last_query: Option<u64> = None;
    for pair in input.chunks(2) {
        let op = pair[0] % 8;
        let arg = u32::from(pair.get(1).copied().unwrap_or(0));
        now = now.saturating_add(u64::from(arg) * 997);
        match op {
            0 => s.set_network(true, now),
            1 => s.set_network(false, now),
            2 => {
                if s.is_due(now) {
                    s.attempted(now);
                    if let Some(prev) = last_query {
                        assert!(
                            now - prev >= MIN_INTERVAL_MS,
                            "queries {prev} and {now} are too close"
                        );
                    }
                    last_query = Some(now);
                }
            }
            3 => s.succeeded(now, arg),
            4 => s.failed(now, arg),
            5 => s.rate_limited(now, arg),
            6 => {
                // The user's button is honoured only when a query could go.
                let honoured = s.sync_now(now);
                if honoured {
                    assert!(s.network());
                }
            }
            _ => {
                now = now.saturating_add(3_600_000);
            }
        }
        if let (Some(due), Some(prev)) = (s.due_at(), last_query) {
            assert!(due >= prev.saturating_add(MIN_INTERVAL_MS));
        }
        assert_eq!(s.due_at().is_some(), s.network());
    }
}

#[cfg(test)]
mod seeded {
    use super::*;
    use crate::packet::build::reply as build_reply;
    use crate::time::Timestamp;
    use fuzzkit::for_seeds;

    /// Replay every checked-in seed (`fuzz/seeds/sntp`) and saved crash
    /// (`fuzz/regressions/sntp`).
    #[test]
    fn corpus_and_regressions_replay() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
        let mut seen = 0;
        for dir in ["seeds", "regressions"] {
            let Ok(entries) = std::fs::read_dir(root.join(dir).join("sntp")) else {
                continue;
            };
            for entry in entries.flatten() {
                run(&std::fs::read(entry.path()).unwrap());
                seen += 1;
            }
        }
        if std::env::var_os("CI").is_some() {
            assert!(seen > 0, "no seeds found for sntp");
        }
    }

    fn input(sent: i64, rtt: u64, packet: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0u8];
        bytes.extend(sent.to_le_bytes());
        bytes.extend(rtt.to_le_bytes());
        bytes.extend(packet);
        bytes
    }

    #[test]
    fn plausible_replies_with_a_flipped_bit_are_safe() {
        const NOW: i64 = 1_791_849_600 * NS_PER_SEC;
        for_seeds("sntp::plausible_replies", |_, rng| {
            let skew = (rng.next_u64() % 4_000_000_000_000) as i64 - 2_000_000_000_000;
            let t2 = NOW + skew + 20_000_000;
            let packet = build_reply(
                rng.next_u64() | 1,
                Timestamp::from_unix_ns(t2),
                Timestamp::from_unix_ns(t2 + 1_000_000),
            );
            let mut bytes = input(NOW, 40_000_000 + rng.next_u64() % 100_000_000, &packet);
            run(&bytes);
            rng.flip_bits(&mut bytes, 3);
            run(&bytes);
        });
    }

    #[test]
    fn raw_noise_is_safe() {
        for_seeds("sntp::raw_noise", |_, rng| {
            let len = rng.range(0, 96) as usize;
            let mut bytes = rng.bytes(len);
            if let Some(first) = bytes.first_mut() {
                *first &= !1;
            }
            run(&bytes);
        });
    }

    #[test]
    fn persistence_scripts_are_safe() {
        for_seeds("sntp::persistence_scripts", |_, rng| {
            let records = rng.range(0, 40) as usize;
            let mut bytes = vec![2u8];
            for _ in 0..records {
                bytes.push(rng.byte() % 80);
                let offset = (rng.next_u64() % 8) as i64 * 4_000_000_000 + rng.range(0, 3) as i64;
                bytes.extend(offset.to_le_bytes());
            }
            run(&bytes);
        });
    }

    #[test]
    fn schedule_scripts_are_safe() {
        for_seeds("sntp::schedule_scripts", |_, rng| {
            let len = rng.range(0, 400) as usize;
            let mut bytes = rng.bytes(len);
            bytes.insert(0, 1);
            run(&bytes);
        });
    }
}
