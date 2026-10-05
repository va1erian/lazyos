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
