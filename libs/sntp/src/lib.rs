//! SNTP for `timed` (`docs/ntp-plan.md`): everything about asking a server
//! for the time that needs no I/O, so the host runs the same code under
//! `cargo test -p sntp`.
//!
//! * [`time`]: NTP 32.32 timestamps and their era-safe conversion to Unix
//!   nanoseconds (the 2036 rollover is not special).
//! * [`packet`]: the 48-byte RFC 4330 request and a validating reply decoder
//!   (the originate echo, mode, version, stratum, kiss-o'-death).
//! * [`sample`]: offset and delay of one exchange, and whether it is fit to
//!   act on.
//! * [`policy`]: whether to step the clock for a sample, and the floor a
//!   clock must not be below.
//! * [`schedule`]: when to ask next (back-off, rate limits, network edges).
//! * [`servers`]: the configured server list.
//!
//! Plain SNTP is unauthenticated: this is a correctness aid for a machine
//! whose RTC is wrong, not a security control, and the policy is written so
//! that an on-path liar can move the clock very little after boot.

#![cfg_attr(not(test), no_std)]

pub mod packet;
pub mod policy;
pub mod sample;
pub mod schedule;
pub mod servers;
pub mod time;

#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;

pub use packet::{decode_reply, encode_request, Kiss, Leap, Reject, Reply, PACKET_LEN};
pub use policy::{decide, floor_step, Decision, Refusal, CLOCK_FLOOR_UNIX};
pub use sample::{best, Sample, Unfit};
pub use schedule::Schedule;
pub use time::Timestamp;

/// The standard NTP port.
pub const PORT: u16 = 123;

/// The server used when none is configured.
pub const DEFAULT_SERVER: &str = "pool.ntp.org";

/// The `confd` key that switches the client on (`true`) or off.
pub const ENABLED_KEY: &str = "sys/time/ntp/enabled";

/// The `confd` key holding the server list: space-separated `host[:port]`.
pub const SERVERS_KEY: &str = "sys/time/ntp/servers";

/// Nanoseconds in a second.
pub const NS_PER_SEC: i64 = 1_000_000_000;
