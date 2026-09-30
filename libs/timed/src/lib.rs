//! Pure time-zone logic for the `timed` service (issue #369).
//!
//! The kernel clock is UTC; this crate turns a UTC instant into local-time
//! parameters (offset, DST flag, abbreviation) for a small built-in zone
//! table. It has no syscall or Messenger dependency, so the host runs the
//! same code under `cargo test -p timezone`.
//!
//! v1 is deliberately tiny: a fixed standard offset plus, for some zones, one
//! recurring DST rule. Full tzdata (historical rule changes) is a follow-up.

#![cfg_attr(not(test), no_std)]

pub mod civil;
pub mod format;
pub mod zones;

pub use zones::{default_zone, find, local, Local, Zone, ZONES};

/// The zone used when `confd` has no `sys/time/zone` value.
pub const DEFAULT_ZONE: &str = "UTC";

/// The `confd` key holding the zone name (root-writable, world-readable).
pub const ZONE_KEY: &str = "sys/time/zone";

/// The retained topic `timed` publishes each minute.
pub const TICK_TOPIC: &str = "time/tick";

/// Seconds between `time/tick` events.
pub const TICK_SECS: i64 = 60;

/// The next tick boundary strictly after `unix` (the top of the next minute).
pub fn next_tick_after(unix: i64) -> i64 {
    unix.div_euclid(TICK_SECS) * TICK_SECS + TICK_SECS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_boundaries() {
        assert_eq!(next_tick_after(0), 60);
        assert_eq!(next_tick_after(59), 60);
        assert_eq!(next_tick_after(60), 120);
        assert_eq!(next_tick_after(-1), 0);
    }

    #[test]
    fn zone_key_is_a_valid_confd_path_root_writable_only() {
        assert!(confd::validate_path(ZONE_KEY).is_ok());
        // `timed` relies on `sys/**` being world-readable and root-writable
        // (see `libs/confd/src/path.rs`).
        let mut store = confd::Store::new();
        let value = confd::Value::Str("UTC".into());
        assert!(store
            .set(ZONE_KEY, value.clone(), confd::Caller { uid: 1000 })
            .is_err());
        assert!(store.set(ZONE_KEY, value, confd::Caller { uid: 0 }).is_ok());
        assert!(store.get(ZONE_KEY, confd::Caller { uid: 1000 }).is_ok());
    }
}
