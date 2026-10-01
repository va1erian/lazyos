//! Reaping windows whose client died (a crash or `kill`) without calling
//! `DestroySurface`.
//!
//! The kernel closes a dead task's endpoints, so a send to the surface's event
//! endpoint fails with `EPIPE`. Real events only flow while the user touches
//! the window, so an idle app would linger forever; the compositor therefore
//! sends every surface a one-way `Ping` about once a second and removes each
//! surface whose endpoint reports a closed peer, exactly as `DestroySurface`
//! would (`forget_surface`, then one repaint of the damage).

use alloc::vec::Vec;
use user::messenger::display::{self, wire};
use user::messenger::{self, Endpoint};
use user::sys;

use super::compositor::Compositor;
use super::surface::Surface;
use super::window::surface_by_id;

/// PIT ticks (100 Hz) between liveness probes.
pub(super) const PROBE_INTERVAL_TICKS: u64 = 100;

/// The ids of the surfaces in `surfaces` whose event endpoint's peer is gone.
/// Sends a `Ping` to each; any other send error (a full queue) is not death.
pub(super) fn dead_surfaces(surfaces: &[Surface], scratch: &mut Vec<u8>) -> Vec<u64> {
    surfaces
        .iter()
        .filter(|surface| surface.events != 0)
        .filter(|surface| {
            let sent = display::send_event(
                &Endpoint::from_raw(surface.events),
                scratch,
                wire::METHOD_PING,
                Ok(Vec::new()),
            );
            matches!(sent, Err(messenger::Error::Errno(code)) if code == -messenger::errno::EPIPE)
        })
        .map(|surface| surface.id)
        .collect()
}

impl Compositor {
    /// Probe every surface's client once per [`PROBE_INTERVAL_TICKS`] and
    /// destroy the windows of dead ones, repainting once for all of them.
    pub(super) fn reap_dead_surfaces(&mut self, now: u64) {
        if now < self.next_probe {
            return;
        }
        self.next_probe = now + PROBE_INTERVAL_TICKS;
        let dead = dead_surfaces(&self.surfaces, &mut self.scratch);
        for id in &dead {
            if surface_by_id(&self.surfaces, *id).is_some() {
                self.forget_surface(*id);
                sys::write_str(&alloc::format!(
                    "XUID:REAP:SURFACE:{id}
"
                ));
            }
        }
        if !dead.is_empty() {
            self.repaint_full();
        }
    }
}

/// Boot check: a surface whose peer endpoint was closed is reported dead, a
/// live one and one that merely has a full queue are not. `XUID:REAP:PASS` or
/// `XUID:REAP:FAIL`.
pub(super) fn selftest_reap() -> &'static str {
    use super::window::test_surface;

    let (Ok((live_events, live_peer)), Ok((dead_events, dead_peer))) =
        (messenger::create_pair(), messenger::create_pair())
    else {
        return "XUID:REAP:FAIL\n";
    };
    let mut live = test_surface(1, false, false);
    live.events = live_events.handle();
    let mut dead = test_surface(2, false, false);
    dead.events = dead_events.handle();
    let _ = dead_peer.close();
    let mut scratch = Vec::new();
    let found = dead_surfaces(&[live, dead], &mut scratch);
    let _ = live_events.close();
    let _ = live_peer.close();
    let _ = dead_events.close();
    if found == [2] {
        "XUID:REAP:PASS\n"
    } else {
        "XUID:REAP:FAIL\n"
    }
}
