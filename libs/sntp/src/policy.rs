//! Whether a measured offset may move the clock.
//!
//! The kernel can only *step* the clock, and an unauthenticated server may
//! be wrong or an attacker, so the rules are narrow (`docs/ntp-plan.md`
//! section 3.4):
//!
//! * the result must not be earlier than [`CLOCK_FLOOR_UNIX`] (nothing real
//!   happened before this software was built) or past the kernel's range;
//! * offsets under [`MIN_STEP_NS`] are left alone (no churn, and below the
//!   wall clock's own 10 ms granularity plus path asymmetry anyway);
//! * the first sync after boot may step by any amount (a dead RTC is exactly
//!   what this exists for), later ones only up to [`MAX_STEP_NS`], so a liar
//!   on the path cannot wander a running clock.

use crate::NS_PER_SEC;

/// No instant before this is real: 2026-01-01T00:00:00Z, the kernel's own
/// fallback for a missing RTC (`kernel/src/wallclock.rs` `FALLBACK_BASE`), so
/// a valid but reset RTC (2000-01-01) is treated like an absent one. A checked-in
/// constant on purpose (a build-time stamp would make every build differ);
/// raise it when a release is cut.
pub const CLOCK_FLOOR_UNIX: i64 = 1_767_225_600;

/// First second the kernel's clock refuses (2200-01-01T00:00:00Z); mirrors
/// `kernel/src/wallclock.rs` `MAX_SET_SECS`.
pub const MAX_UNIX_SECS: i64 = 7_258_118_400;

/// Smaller offsets are not corrected.
pub const MIN_STEP_NS: i64 = 250_000_000;

/// The largest step after the first sync of a boot.
pub const MAX_STEP_NS: i64 = 1_000 * NS_PER_SEC;

/// How many agreeing refusals make a large offset believable.
pub const PERSIST_COUNT: u32 = 3;
/// How long they must span, milliseconds.
pub const PERSIST_MIN_MS: u64 = 600_000;
/// How far apart two of them may be and still agree.
pub const PERSIST_AGREE_NS: i64 = 5 * NS_PER_SEC;

/// Evidence that an offset beyond [`MAX_STEP_NS`] is real, not one lying
/// server: [`PERSIST_COUNT`] refusals in a row, at least [`PERSIST_MIN_MS`]
/// apart overall, all within [`PERSIST_AGREE_NS`] of each other. A machine
/// that was paused or restored (or whose clock was set far off with automatic
/// time on) is then corrected instead of refused until the next boot. The
/// caller resets it on any other outcome.
#[derive(Clone, Debug, Default)]
pub struct Persistence {
    count: u32,
    first_ms: u64,
    offset_ns: i64,
}

impl Persistence {
    pub const fn new() -> Persistence {
        Persistence {
            count: 0,
            first_ms: 0,
            offset_ns: 0,
        }
    }

    /// Record one too-large offset seen at `now_ms`; whether it is now
    /// believable.
    pub fn observe(&mut self, now_ms: u64, offset_ns: i64) -> bool {
        let agrees = self.count > 0
            && i128::from(offset_ns).abs_diff(i128::from(self.offset_ns))
                <= PERSIST_AGREE_NS as u128;
        if agrees {
            self.count = self.count.saturating_add(1);
        } else {
            self.count = 1;
            self.first_ms = now_ms;
        }
        self.offset_ns = offset_ns;
        self.count >= PERSIST_COUNT && now_ms.saturating_sub(self.first_ms) >= PERSIST_MIN_MS
    }

    pub fn reset(&mut self) {
        *self = Persistence::new();
    }
}

/// Why a sample was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The corrected time would be earlier than [`CLOCK_FLOOR_UNIX`].
    BeforeFloor,
    /// The corrected time is outside the kernel's range.
    BeyondRange,
    /// Beyond [`MAX_STEP_NS`] and this is not the first sync of the boot.
    TooLarge,
}

/// What to do with a sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Close enough: leave the clock.
    Keep,
    /// Step the clock to this Unix instant (nanoseconds).
    Step {
        target_unix_ns: i64,
    },
    Refuse(Refusal),
}

/// Judge a sample whose offset is `offset_ns` against a local clock that
/// reads `now_unix_ns`. `first_sync` is true until one exchange has
/// succeeded since boot.
pub fn decide(now_unix_ns: i64, offset_ns: i64, first_sync: bool) -> Decision {
    let target = i128::from(now_unix_ns) + i128::from(offset_ns);
    if target < i128::from(CLOCK_FLOOR_UNIX) * i128::from(NS_PER_SEC) {
        return Decision::Refuse(Refusal::BeforeFloor);
    }
    if target >= i128::from(MAX_UNIX_SECS) * i128::from(NS_PER_SEC) {
        return Decision::Refuse(Refusal::BeyondRange);
    }
    let size = i128::from(offset_ns).abs();
    if size < i128::from(MIN_STEP_NS) {
        return Decision::Keep;
    }
    if !first_sync && size > i128::from(MAX_STEP_NS) {
        return Decision::Refuse(Refusal::TooLarge);
    }
    // In range by the checks above, so the cast cannot truncate.
    Decision::Step {
        target_unix_ns: target as i64,
    }
}

/// At service start: the instant to step a clock that reads `now_unix_secs`
/// up to, when it reads earlier than [`CLOCK_FLOOR_UNIX`] (a dead or reset
/// RTC). Needs no network.
pub fn floor_step(now_unix_secs: i64) -> Option<i64> {
    (now_unix_secs < CLOCK_FLOOR_UNIX).then_some(CLOCK_FLOOR_UNIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_791_849_600 * NS_PER_SEC;
    const MS: i64 = 1_000_000;

    #[test]
    fn small_offsets_are_kept() {
        for offset in [0, 1, 249 * MS, -249 * MS] {
            assert_eq!(decide(NOW, offset, true), Decision::Keep);
            assert_eq!(decide(NOW, offset, false), Decision::Keep);
        }
    }

    #[test]
    fn the_step_threshold_is_inclusive() {
        for offset in [250 * MS, -250 * MS] {
            assert_eq!(
                decide(NOW, offset, false),
                Decision::Step {
                    target_unix_ns: NOW + offset
                }
            );
        }
    }

    #[test]
    fn later_syncs_are_bounded() {
        let ok = 1_000 * NS_PER_SEC;
        assert!(matches!(decide(NOW, ok, false), Decision::Step { .. }));
        assert!(matches!(decide(NOW, -ok, false), Decision::Step { .. }));
        assert_eq!(
            decide(NOW, ok + 1, false),
            Decision::Refuse(Refusal::TooLarge)
        );
        assert_eq!(
            decide(NOW, -ok - 1, false),
            Decision::Refuse(Refusal::TooLarge)
        );
    }

    #[test]
    fn the_first_sync_takes_a_dead_rtc_forward() {
        // A reset RTC (2000-01-01) and a server a quarter century ahead.
        let rtc = 946_684_800 * NS_PER_SEC;
        let offset = NOW - rtc;
        assert_eq!(
            decide(rtc, offset, true),
            Decision::Step {
                target_unix_ns: NOW
            }
        );
        assert_eq!(
            decide(rtc, offset, false),
            Decision::Refuse(Refusal::TooLarge)
        );
    }

    #[test]
    fn nothing_goes_before_the_floor_or_past_the_range() {
        let floor = CLOCK_FLOOR_UNIX * NS_PER_SEC;
        // A hostile server answering with 1970 (or a hair under the floor).
        for first in [true, false] {
            assert_eq!(
                decide(NOW, -NOW, first),
                Decision::Refuse(Refusal::BeforeFloor)
            );
            assert_eq!(
                decide(NOW, floor - 1 - NOW, first),
                Decision::Refuse(Refusal::BeforeFloor)
            );
        }
        assert!(matches!(
            decide(NOW, floor - NOW, true),
            Decision::Step { .. } | Decision::Keep
        ));
        let max = MAX_UNIX_SECS * NS_PER_SEC;
        assert_eq!(
            decide(NOW, max - NOW, true),
            Decision::Refuse(Refusal::BeyondRange)
        );
        assert!(matches!(
            decide(NOW, max - 1 - NOW, true),
            Decision::Step { .. }
        ));
    }

    #[test]
    fn three_agreeing_refusals_over_ten_minutes_are_believed() {
        let mut p = Persistence::new();
        let hours = 3 * 3_600 * NS_PER_SEC;
        assert!(!p.observe(0, hours));
        assert!(!p.observe(300_000, hours + NS_PER_SEC));
        // Three, but not yet ten minutes.
        assert!(!p.observe(599_999, hours + 2 * NS_PER_SEC));
        assert!(p.observe(600_000, hours + 3 * NS_PER_SEC));
        assert!(p.observe(900_000, hours));
    }

    #[test]
    fn disagreeing_or_reset_evidence_starts_over() {
        let mut p = Persistence::new();
        assert!(!p.observe(0, 3_600 * NS_PER_SEC));
        assert!(!p.observe(400_000, 3_600 * NS_PER_SEC));
        // A server that says something else restarts the count.
        assert!(!p.observe(800_000, 7_200 * NS_PER_SEC));
        assert!(!p.observe(1_200_000, 7_200 * NS_PER_SEC));
        assert!(
            !p.observe(1_300_000, 7_200 * NS_PER_SEC),
            "only 500 s since it changed"
        );
        p.reset();
        assert!(!p.observe(5_000_000, 7_200 * NS_PER_SEC));
    }

    #[test]
    fn extreme_values_do_not_panic() {
        for now in [i64::MIN, -1, 0, NOW, i64::MAX] {
            for offset in [i64::MIN, -1, 0, 1, i64::MAX] {
                for first in [true, false] {
                    let _ = decide(now, offset, first);
                }
            }
        }
    }

    #[test]
    fn floor_step_only_below_the_floor() {
        assert_eq!(floor_step(0), Some(CLOCK_FLOOR_UNIX));
        assert_eq!(floor_step(946_684_800), Some(CLOCK_FLOOR_UNIX));
        assert_eq!(floor_step(CLOCK_FLOOR_UNIX - 1), Some(CLOCK_FLOOR_UNIX));
        assert_eq!(floor_step(CLOCK_FLOOR_UNIX), None);
        assert_eq!(floor_step(i64::MAX), None);
    }

    #[test]
    fn the_floor_is_a_real_past_date() {
        // 2026-01-01T00:00:00Z, the kernel's fallback base.
        assert_eq!(CLOCK_FLOOR_UNIX, 1_767_225_600);
    }
}
