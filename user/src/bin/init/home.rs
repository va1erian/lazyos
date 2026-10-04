//! Waiting for a home volume that appears after boot: a USB stick served by
//! `usbd` (docs/architecture/usb-storage.md).
//!
//! When `lazyos.cfg` names a home volume the kernel did not find at boot, the
//! kernel keeps the request and mounts the volume at `/home` once a block
//! provider's disk carries it (syscall 33 `SETTLE`). `init` asks on every
//! supervision-loop pass and holds back the rows that use `/home` (accounts
//! and logins, and the desktop's autostart apps) until the answer is final:
//! mounted, nothing pending, absent (every provider finished its first scan
//! without finding it), no provider running, or [`WAIT_TICKS`] passed. The
//! wait is bounded, so a stick that never answers costs boot time, never the
//! boot.
//!
//! Images without the USB driver have no provider: nothing is held there.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use alloc::format;
use user::sys::{self, settle_state};

use super::service::{Phase, Service};

/// The longest the session waits for a late home volume (100 Hz): 60 s. A
/// stick is found in a few seconds on real hardware, and the wait ends as
/// soon as `usbd` has looked at every port; the bound only matters when it
/// hangs, or under TCG on a loaded host (two sticks took over 30 s there).
const WAIT_TICKS: u64 = 6000;
/// How often the kernel is asked while waiting.
pub(super) const POLL_TICKS: u64 = 10;

/// Rows that read or create files under `/home` when they start.
const NEEDS_HOME: &[&str] = &["accountsd", "logind"];

/// Whether the wait is over (from the start without a provider).
static DONE: AtomicBool = AtomicBool::new(!cfg!(lazyos_usb));
/// When the wait gives up; 0 until the first step.
static DEADLINE: AtomicU64 = AtomicU64::new(0);

/// Whether rows that use `/home` may start.
pub(super) fn ready() -> bool {
    DONE.load(Ordering::Relaxed)
}

/// Whether row `name` is held back for now.
pub(super) fn holds(name: &str) -> bool {
    !ready() && NEEDS_HOME.contains(&name)
}

/// The supervision loop's wake-up while waiting: soon enough to ask again.
pub(super) fn wake(now: u64) -> Option<u64> {
    (!ready()).then_some(now + POLL_TICKS)
}

/// Ask the kernel once; returns true when this call ended the wait (the
/// caller then starts what was held).
pub(super) fn step(services: &[Service], now: u64) -> bool {
    if ready() {
        return false;
    }
    let deadline = match DEADLINE.load(Ordering::Relaxed) {
        0 => {
            DEADLINE.store(now + WAIT_TICKS, Ordering::Relaxed);
            now + WAIT_TICKS
        }
        deadline => deadline,
    };
    let provider = services.iter().any(|row| {
        row.name == "usbd"
            && matches!(
                row.phase,
                Phase::Pending | Phase::Running | Phase::Restarting
            )
    });
    let why = match sys::storage_settle() {
        Ok(settle_state::MOUNTED) => "mounted",
        Ok(settle_state::MOUNTED_EARLIER) => "mounted earlier",
        Ok(settle_state::NONE) => "nothing pending",
        Ok(settle_state::ABSENT) => "absent",
        Ok(settle_state::WAITING) if !provider => "no provider",
        Ok(settle_state::WAITING) if now >= deadline => "timed out",
        Ok(settle_state::WAITING) => return false,
        Ok(_) => "unknown state",
        Err(_) => "settle refused",
    };
    DONE.store(true, Ordering::Relaxed);
    sys::write_str(&format!("INIT:HOME {why}\n"));
    true
}
