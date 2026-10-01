//! Session owners `init` learned from `logind` (issue #157).
//!
//! A root `Launch` into another session stamps the child with that session's
//! uid, which `init` used to ask `logind` for (`Sessions`). That cannot work
//! when `logind` itself is the caller: a graphical login asks `init` to launch
//! LazyShell into the new session while `logind` is blocked in that call, so
//! `init`'s query back to `logind` would only time out. Instead `init` watches
//! the login events `logind` already publishes through this supervisor's topic
//! broker (`system/events/login/session/<id>`, `system/events/login/end`) and
//! remembers each live session's uid.
//!
//! Only events from the running `logind` manifest row's own task are believed:
//! the broker accepts publishes from anyone, and a forged owner would make a
//! later root launch stamp the wrong uid. A forged event is ignored, not
//! reported, since publishing is not itself a privileged act.

use core::sync::atomic::{AtomicU64, Ordering};

use user::messenger::{logind, router, Message};

use super::service::{Phase, Service};

/// Live sessions remembered at once; a full table forgets the oldest entry,
/// which then falls back to asking `logind`.
const SLOTS: usize = 16;

/// Session id per slot (`0` = free).
static SESSION: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
/// The owning uid for the session in the same slot.
static UID: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
/// The slot the next new session overwrites when none is free.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// The uid that owns `session`, if `logind` announced it.
pub(super) fn owner(session: u64) -> Option<u32> {
    if session == 0 {
        return None;
    }
    (0..SLOTS)
        .find(|&slot| SESSION[slot].load(Ordering::Relaxed) == session)
        .map(|slot| UID[slot].load(Ordering::Relaxed) as u32)
}

/// Learn from one inbound message, if it is a login event `logind` published.
pub(super) fn observe(services: &[Service], message: &Message) {
    let Some((topic, payload)) = router::published(message) else {
        return;
    };
    if !from_logind(services, message.sender) {
        return;
    }
    if topic == logind::wire::TOPIC_SYSTEM_EVENTS_LOGIN_END {
        if let Ok(end) = logind::wire::decode_login_end(&payload) {
            forget(end.session);
        }
        return;
    }
    // `system/events/login/session/<id>`: the id is the last segment.
    let prefix = logind::wire::TOPIC_SYSTEM_EVENTS_LOGIN_SESSION.trim_end_matches('+');
    let Some(id) = topic
        .strip_prefix(prefix)
        .and_then(|id| id.parse::<u64>().ok())
    else {
        return;
    };
    let Ok(record) = logind::wire::decode_login_session(&payload) else {
        return;
    };
    if record.state == "exited" {
        forget(id);
    } else {
        remember(id, record.uid);
    }
}

/// Whether `sender` is the task of the running `logind` manifest row.
fn from_logind(services: &[Service], sender: u64) -> bool {
    services.iter().any(|service| {
        !service.launched
            && service.name == "logind"
            && service.phase == Phase::Running
            && service.pid != 0
            && service.pid == sender
    })
}

/// Record `session` as owned by `uid`, reusing its slot or a free one.
fn remember(session: u64, uid: u32) {
    if session == 0 {
        return;
    }
    let slot = (0..SLOTS)
        .find(|&slot| SESSION[slot].load(Ordering::Relaxed) == session)
        .or_else(|| (0..SLOTS).find(|&slot| SESSION[slot].load(Ordering::Relaxed) == 0))
        .unwrap_or_else(|| (NEXT.fetch_add(1, Ordering::Relaxed) as usize) % SLOTS);
    UID[slot].store(uid as u64, Ordering::Relaxed);
    SESSION[slot].store(session, Ordering::Relaxed);
}

/// Drop `session` (it ended).
fn forget(session: u64) {
    for slot in &SESSION {
        if slot.load(Ordering::Relaxed) == session {
            slot.store(0, Ordering::Relaxed);
        }
    }
}

/// The self-test: an event from a task that is not `logind` is ignored, one
/// from `logind` is remembered and an end forgets it again. Exercised through
/// the table directly (a synthetic `Message` would need a real sender).
/// Prints `INIT:SESSIONS:PASS`.
pub(super) fn selftest() -> &'static str {
    const PROBE: u64 = 0xfeed_0001;
    let before = owner(PROBE).is_none();
    remember(PROBE, 4321);
    let learned = owner(PROBE) == Some(4321);
    remember(PROBE, 1234);
    let updated = owner(PROBE) == Some(1234);
    forget(PROBE);
    let forgotten = owner(PROBE).is_none() && owner(0).is_none();
    let refused = !from_logind(&[], 1);
    if before && learned && updated && forgotten && refused {
        "INIT:SESSIONS:PASS\n"
    } else {
        "INIT:SESSIONS:FAIL session owner table broke\n"
    }
}
