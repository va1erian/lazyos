//! One exchange as an offset and a delay, and whether it is worth acting on.
//!
//! With `t1` the client's clock when the request left, `t2` and `t3` the
//! server's receive and transmit times and `t4` the client's clock when the
//! reply arrived (RFC 4330 section 5):
//!
//! ```text
//! offset = ((t2 - t1) + (t3 - t4)) / 2        server minus client
//! delay  = (t4 - t1) - (t3 - t2)
//! ```
//!
//! The caller's wall clock ticks every 10 ms, so `t4` is not read from it:
//! the round trip is measured on the monotonic clock and `t4 = t1 + rtt`.
//! What stays is the wall clock's own 10 ms granularity at `t1`, which is
//! the accuracy this client claims (tens of milliseconds).

use crate::packet::Reply;

/// A round trip longer than this is not a measurement worth stepping a
/// clock for: the offset's error bound is half the delay.
pub const MAX_DELAY_NS: i64 = 1_000_000_000;

/// RFC 5905's `MAXDIST` is 1.5 s; chrony defaults to 3 s. A server further
/// than this from its reference is no better than a guess.
pub const MAX_ROOT_DISTANCE_NS: u64 = 3_000_000_000;

/// How negative a computed delay may be (clock jitter between two machines)
/// before the sample is thrown away rather than clamped to zero.
const DELAY_SLACK_NS: i64 = 10_000_000;

/// Why a sample is not fit to act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unfit {
    /// Negative beyond the slack, or longer than [`MAX_DELAY_NS`].
    Delay,
    /// The server is too far from its own reference.
    RootDistance,
}

/// One accepted exchange.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    /// Server minus client, nanoseconds: add it to the local clock.
    pub offset_ns: i64,
    pub delay_ns: i64,
    pub stratum: u8,
    pub root_distance_ns: u64,
}

fn clamp(value: i128) -> i64 {
    value.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

impl Sample {
    /// Measure `reply`, which answered a request sent when the local clock
    /// read `sent_unix_ns` and that took `rtt_ns` (monotonic) to come back.
    pub fn measure(reply: &Reply, sent_unix_ns: i64, rtt_ns: u64) -> Result<Sample, Unfit> {
        let t1 = i128::from(sent_unix_ns);
        let t4 = t1 + i128::from(rtt_ns);
        let t2 = i128::from(reply.receive.to_unix_ns(sent_unix_ns));
        let t3 = i128::from(reply.transmit.to_unix_ns(sent_unix_ns));
        let offset = ((t2 - t1) + (t3 - t4)) / 2;
        let delay = clamp((t4 - t1) - (t3 - t2));
        if !(-DELAY_SLACK_NS..=MAX_DELAY_NS).contains(&delay) {
            return Err(Unfit::Delay);
        }
        let delay = delay.max(0);
        let root_distance = reply
            .root_delay_ns
            .saturating_div(2)
            .saturating_add(reply.root_dispersion_ns)
            .saturating_add(delay as u64 / 2);
        if root_distance > MAX_ROOT_DISTANCE_NS {
            return Err(Unfit::RootDistance);
        }
        Ok(Sample {
            offset_ns: clamp(offset),
            delay_ns: delay,
            stratum: reply.stratum,
            root_distance_ns: root_distance,
        })
    }
}

/// The index of the sample with the least delay (the smallest error bound),
/// the first on a tie; `None` for no samples.
pub fn best(samples: &[Sample]) -> Option<usize> {
    samples
        .iter()
        .enumerate()
        .min_by_key(|(_, sample)| sample.delay_ns)
        .map(|(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::build::reply;
    use crate::packet::{decode_reply, Leap};
    use crate::time::Timestamp;
    use crate::NS_PER_SEC;
    use core::num::NonZeroU64;

    /// 2026-10-13T00:00:00Z in nanoseconds.
    const T1: i64 = 1_791_849_600 * NS_PER_SEC;
    const MS: i64 = 1_000_000;

    fn nonce() -> NonZeroU64 {
        NonZeroU64::new(1).unwrap()
    }

    /// The reply of a server whose clock reads client + `ahead`; the request
    /// takes `one_way` to arrive and the server holds it `processing`.
    fn exchange(ahead: i64, one_way: i64, processing: i64) -> Reply {
        let t2 = T1 + one_way + ahead;
        let t3 = t2 + processing;
        let packet = reply(1, Timestamp::from_unix_ns(t2), Timestamp::from_unix_ns(t3));
        decode_reply(&packet, nonce()).unwrap()
    }

    fn rtt(ms: i64) -> u64 {
        (ms * MS) as u64
    }

    #[test]
    fn symmetric_path_measures_the_offset() {
        for ahead in [
            0,
            10 * MS,
            -10 * MS,
            3_600 * NS_PER_SEC,
            -86_400 * NS_PER_SEC,
        ] {
            let r = exchange(ahead, 20 * MS, 2 * MS);
            let s = Sample::measure(&r, T1, rtt(42)).unwrap();
            assert!((s.offset_ns - ahead).abs() <= 4, "{ahead}: {}", s.offset_ns);
            assert!((s.delay_ns - 40 * MS).abs() <= 4, "{}", s.delay_ns);
        }
    }

    #[test]
    fn asymmetry_bounds_the_error_by_half_the_delay() {
        // All 40 ms of the path on the way out: the offset reads 20 ms high.
        let r = exchange(0, 40 * MS, 0);
        let s = Sample::measure(&r, T1, rtt(40)).unwrap();
        assert!((s.offset_ns - 20 * MS).abs() <= 4);
        assert!(s.offset_ns.abs() <= s.delay_ns / 2 + 4);
    }

    #[test]
    fn a_slow_round_trip_is_unfit() {
        let r = exchange(0, 600 * MS, 0);
        assert_eq!(Sample::measure(&r, T1, rtt(1_200)), Err(Unfit::Delay));
    }

    #[test]
    fn a_server_claiming_to_have_held_it_longer_than_the_trip_is_unfit() {
        let r = exchange(0, 10 * MS, 500 * MS);
        assert_eq!(Sample::measure(&r, T1, rtt(20)), Err(Unfit::Delay));
    }

    #[test]
    fn a_few_ms_of_negative_delay_is_clamped_not_discarded() {
        // Held 25 ms by the server's clock, 20 ms by ours.
        let r = exchange(0, 0, 25 * MS);
        let s = Sample::measure(&r, T1, rtt(20)).unwrap();
        assert_eq!(s.delay_ns, 0);
    }

    #[test]
    fn root_distance_is_bounded() {
        let mut packet = reply(1, Timestamp::from_unix_ns(T1), Timestamp::from_unix_ns(T1));
        // A root dispersion of 4 s.
        packet[8..12].copy_from_slice(&0x0004_0000u32.to_be_bytes());
        let r = decode_reply(&packet, nonce()).unwrap();
        assert_eq!(Sample::measure(&r, T1, 0), Err(Unfit::RootDistance));
        assert_eq!(r.leap, Leap::None);
    }

    #[test]
    fn works_across_the_era_rollover() {
        // The client reads six seconds before the NTP seconds wrap, the
        // server one second after.
        let rollover = (1i64 << 32) - crate::time::NTP_UNIX_OFFSET;
        let t1 = (rollover - 6) * NS_PER_SEC;
        let t2 = (rollover + 1) * NS_PER_SEC;
        let packet = reply(1, Timestamp::from_unix_ns(t2), Timestamp::from_unix_ns(t2));
        let r = decode_reply(&packet, nonce()).unwrap();
        assert_eq!(r.receive.seconds(), 1);
        let s = Sample::measure(&r, t1, 0).unwrap();
        assert_eq!(s.offset_ns / NS_PER_SEC, 7);
    }

    #[test]
    fn extreme_inputs_do_not_panic() {
        let r = exchange(0, 0, 0);
        for sent in [i64::MIN, -1, 0, i64::MAX] {
            for rtt in [0, 1, u64::MAX] {
                let _ = Sample::measure(&r, sent, rtt);
            }
        }
    }

    #[test]
    fn best_is_the_least_delay() {
        let s = |delay_ns| Sample {
            offset_ns: 0,
            delay_ns,
            stratum: 2,
            root_distance_ns: 0,
        };
        assert_eq!(best(&[]), None);
        assert_eq!(best(&[s(30), s(10), s(20), s(10)]), Some(1));
    }
}
