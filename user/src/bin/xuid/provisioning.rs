//! Before a desktop image's compositor takes the screen, the core packages
//! are installed.
//!
//! `pkgd` provisions the core packages at its start (a fresh image, or a
//! rebuild that changed them): a dozen archives extracted and written to
//! disk. Done under an open desktop, that work made its first seconds
//! sluggish, so it now happens at the console, before the session: `init`
//! holds its autostart until `pkgd` announces the pass is over
//! (`user/src/bin/init/autostart.rs`), and `xuid`, which the kernel starts
//! with the services, leaves the console on screen until then, printing how
//! far `pkgd` got.
//!
//! The wait is bounded so a stuck `pkgd` never costs the desktop:
//! [`APPEAR_TICKS`] for `pkgd` to register at all (an image without one
//! does not wait longer), then [`WAIT_TICKS`] in total, the same bound as
//! `init`'s. Serial evidence: `XUID:PROVISION:WAIT`, then
//! `XUID:PROVISION:DONE installed=<n> upgraded=<n> failed=<n>`,
//! `XUID:PROVISION:ABSENT` or `XUID:PROVISION:TIMEOUT`.

use alloc::format;
use user::messenger::{self, pkgd};
use user::sys;

/// Ticks (100 Hz) `pkgd` may take to register its name: 20 s.
const APPEAR_TICKS: u64 = 2_000;
/// Ticks the whole wait may take: 3 min, `init`'s `PROVISION_WAIT`.
const WAIT_TICKS: u64 = 18_000;
/// Ticks one `Provisioned` call may wait: `pkgd` answers between two
/// packages, and writing one can take a few seconds.
const CALL_TICKS: u64 = 1_000;
/// Nanoseconds between two progress polls.
const POLL_NS: u64 = 250_000_000;

/// Wait until `pkgd` has provisioned the core packages, or the bounds ran out.
pub(super) fn wait_for_core_packages() {
    let start = sys::clock();
    let Some(client) = connect(start + APPEAR_TICKS) else {
        sys::write_str("XUID:PROVISION:ABSENT no pkgd, opening the desktop\n");
        return;
    };
    sys::write_str("XUID:PROVISION:WAIT installing the core apps\n");
    let mut client = client;
    let mut shown = u64::MAX;
    loop {
        let now = sys::clock();
        if now >= start + WAIT_TICKS {
            sys::write_str("XUID:PROVISION:TIMEOUT opening the desktop\n");
            return;
        }
        match client.provisioned_until(Some(now + CALL_TICKS)) {
            Ok(state) if state.done => {
                sys::write_str(&format!(
                    "XUID:PROVISION:DONE installed={} upgraded={} failed={}\n",
                    state.installed, state.upgraded, state.failed
                ));
                return;
            }
            Ok(state) => {
                let progress = state.installed + state.upgraded;
                if progress != shown {
                    shown = progress;
                    sys::write_str(&format!("xuid: core apps installed so far: {progress}\n"));
                }
            }
            // `pkgd` restarted (it recycles its memory) or was too busy to
            // answer in time: find it again and keep asking.
            Err(_) => {
                if let Some(again) = connect(sys::clock() + CALL_TICKS) {
                    client = again;
                }
            }
        }
        let _ = sys::sleep_until_ns(sys::monotonic_ns() + POLL_NS);
    }
}

/// Longest hold `diag.hold` may ask for (the build clamps it too).
const MAX_HOLD_SECS: u64 = 600;

/// Seconds named by a `diag.hold=<n>` line of `lazyos.cfg`, 0 when absent or
/// not a positive whole number.
fn diag_hold_secs(cfg: &str) -> u64 {
    cfg.lines()
        .filter_map(|line| line.trim().strip_prefix("diag.hold="))
        .find_map(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(0)
        .min(MAX_HOLD_SECS)
}

/// A stick built with `LAZYOS_DIAG_HOLD=<seconds>` keeps the console panes on
/// screen that long before the desktop takes the screen, so the driver lines
/// printed after the core apps (`USBD:*`, `NETDRV:*`, ...) can be read and
/// photographed on a PC with no serial port. Everything else opens at once.
pub(super) fn hold_for_diagnosis() {
    let Ok(bytes) = user::files::read_up_to("/boot/lazyos.cfg", 4096) else {
        return;
    };
    let seconds = diag_hold_secs(core::str::from_utf8(&bytes).unwrap_or(""));
    if seconds == 0 {
        return;
    }
    sys::write_str(&format!(
        "XUID:DIAG:HOLD {seconds} s: the desktop opens after this (diag.hold in lazyos.cfg)\n"
    ));
    let end = sys::monotonic_ns() + seconds * 1_000_000_000;
    let mut next_note = 0;
    loop {
        let now = sys::monotonic_ns();
        if now >= end {
            break;
        }
        let left = (end - now) / 1_000_000_000;
        if left / 10 != next_note {
            next_note = left / 10;
            sys::write_str(&format!("XUID:DIAG:HOLD {left} s left\n"));
        }
        let _ = sys::sleep_until_ns(now + 500_000_000);
    }
    sys::write_str("XUID:DIAG:HOLD over, opening the desktop\n");
}

/// A `pkgd` client, retrying until the absolute tick `until`.
fn connect(until: u64) -> Option<pkgd::Client> {
    loop {
        if let Ok(client) = pkgd::Client::connect() {
            return Some(client);
        }
        if sys::clock() >= until {
            return None;
        }
        messenger::park_tick();
    }
}
