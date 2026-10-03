//! What `init` knows of `pkgd`'s core package provisioning (issue #509), for
//! the autostart: on a fresh image the apps a session opens are installed by
//! `pkgd` at its first start, so they cannot be launched before it.
//!
//! `pkgd` announces progress on `system/events/pkg/provision` through this
//! supervisor's topic broker: `ready` once the packages that open at login are
//! installed (it provisions them first), `done ...` when the pass is over.
//! `init` watches those publishes instead of calling `pkgd.Provisioned()`:
//! `pkgd` answers requests only between two packages, and writing one to disk
//! can take seconds, so a call short enough not to stall the supervisor would
//! rarely be answered. Only publishes from the running `pkgd` manifest row's
//! own task count, as `sessions.rs` does for `logind`.

use core::sync::atomic::{AtomicU8, Ordering};

use user::messenger::pkgd::wire;
use user::messenger::{router, Message};

use super::service::{Phase, Service};

/// Nothing heard yet.
const UNKNOWN: u8 = 0;
/// The packages that open at login are installed.
const READY: u8 = 1;
/// The pass is over.
const DONE: u8 = 2;

static STATE: AtomicU8 = AtomicU8::new(UNKNOWN);

/// Whether the apps a session opens are installed (or the pass is over).
pub(super) fn ready() -> bool {
    STATE.load(Ordering::Relaxed) >= READY
}

/// Whether this boot's provisioning pass is over.
pub(super) fn done() -> bool {
    STATE.load(Ordering::Relaxed) == DONE
}

/// Learn from one inbound message, if it is `pkgd`'s provisioning event.
pub(super) fn observe(services: &[Service], message: &Message) {
    let Some((topic, payload)) = router::published(message) else {
        return;
    };
    if wire::name_system_events_pkg("provision").ok().as_deref() != Some(topic.as_str()) {
        return;
    }
    if !from_pkgd(services, message.sender) {
        return;
    }
    let Ok(event) = wire::decode_pkg_event(&payload) else {
        return;
    };
    // Per-package records carry a `system_name`; the progress events do not.
    if !event.system_name.is_empty() {
        return;
    }
    let state = match event.detail.split_whitespace().next() {
        Some("ready") => READY,
        Some("done") => DONE,
        _ => return,
    };
    STATE.fetch_max(state, Ordering::Relaxed);
}

/// Whether `sender` is the task of the running `pkgd` manifest row.
fn from_pkgd(services: &[Service], sender: u64) -> bool {
    services.iter().any(|service| {
        !service.launched
            && service.name == "pkgd"
            && service.phase == Phase::Running
            && service.pid != 0
            && service.pid == sender
    })
}
