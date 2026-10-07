//! Session owners `init` learned from `logind` (issues #157, #508).
//!
//! A root `Launch` into another session stamps the child with that session's
//! uid, which `init` used to ask `logind` for (`Sessions`). That cannot work
//! when `logind` itself is the caller: a graphical login asks `init` to launch
//! LazyShell into the new session while `logind` is blocked in that call, so
//! `init`'s query back to `logind` would only time out. Instead `init` watches
//! the login events `logind` already publishes through this supervisor's topic
//! broker (`system/events/login/session/<id>`, `system/events/login/end`) and
//! remembers each live session's owner: its uid, account name and home.
//!
//! The name and home are the session's environment (`HOME`, `USER`, `PATH`,
//! [`env`]) for every app `init` launches into it. `logind` looked the account
//! up once at login and the event carries it, so a launch never asks
//! `accountsd` again. A session no login announced (the desktop's boot-time
//! session 0, run as uid 0) is resolved by one `accountsd` lookup per uid,
//! remembered for the rest of the boot.
//!
//! Only events from the running `logind` manifest row's own task are believed:
//! the broker accepts publishes from anyone, and a forged owner would make a
//! later root launch stamp the wrong uid. A forged event is ignored, not
//! reported, since publishing is not itself a privileged act.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use spin::Mutex;
use user::messenger::{accounts, logind, registry, router, Message};
use user::sys;

use super::service::{Phase, Service};

/// Live sessions remembered at once; a full table forgets the oldest entry,
/// which then falls back to asking `logind`.
const SLOTS: usize = 16;
/// How long one `accountsd` lookup may take (PIT ticks, 100 Hz).
const LOOKUP_TICKS: u64 = 50;

/// Who owns a session: what `logind` announced for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Owner {
    pub(super) uid: u32,
    pub(super) user: String,
    pub(super) home: String,
}

/// One remembered session.
struct Slot {
    session: u64,
    owner: Owner,
}

/// The live sessions (`init` is one task; the lock only makes the static safe).
static TABLE: Mutex<Vec<Slot>> = Mutex::new(Vec::new());
/// The slot the next new session overwrites when the table is full.
static NEXT: AtomicU64 = AtomicU64::new(0);
/// Accounts resolved by uid for sessions no login announced: `None` caches
/// "no such account" so a missing one is not looked up on every launch.
static BY_UID: Mutex<Vec<(u32, Option<Owner>)>> = Mutex::new(Vec::new());

/// The uid that owns `session`, if `logind` announced it.
pub(super) fn owner(session: u64) -> Option<u32> {
    account(session).map(|owner| owner.uid)
}

/// The owner of `session`, if `logind` announced it.
fn account(session: u64) -> Option<Owner> {
    if session == 0 {
        return None;
    }
    TABLE
        .lock()
        .iter()
        .find(|slot| slot.session == session)
        .map(|slot| slot.owner.clone())
}

/// The environment of an app launched into `session` as `uid`: the session's
/// `HOME`, `USER` and `PATH` ([`accounts::session_env`]). The account is the
/// one `logind` announced for the session or, for a session it did not (the
/// boot-time session 0), the one `accountsd` has for `uid`. Without an account
/// (no account file: `accountsd` failed closed) only `PATH` is set.
pub(super) fn env(session: u64, uid: u32) -> Vec<String> {
    let owner = account(session)
        .filter(|owner| owner.uid == uid)
        .or_else(|| by_uid(uid));
    match owner {
        Some(owner) => accounts::session_env(&owner.user, &owner.home).into(),
        None => alloc::vec![alloc::format!("PATH={}", fhs::SYSTEM_BIN)],
    }
}

/// The account of `uid`, from the cache or one `accountsd` lookup. A failed
/// lookup (service down or without accounts) is not cached, so the account
/// appears once `accountsd` can answer.
fn by_uid(uid: u32) -> Option<Owner> {
    if let Some((_, cached)) = BY_UID.lock().iter().find(|(seen, _)| *seen == uid) {
        return cached.clone();
    }
    let endpoint = registry::resolve(accounts::NAME).ok()?;
    let deadline = Some(sys::clock() + LOOKUP_TICKS);
    let found = accounts::lookup_uid_by(&endpoint, uid, deadline).ok()?;
    let owner = found.map(|record| Owner {
        uid: record.uid,
        user: record.name,
        home: record.home,
    });
    BY_UID.lock().push((uid, owner.clone()));
    owner
}

/// Learn from one inbound message, if it is a login event `logind` published.
/// Returns the session that just ended (a logout, issue #623), whose tasks
/// the caller must end.
pub(super) fn observe(services: &[Service], message: &Message) -> Option<u64> {
    let (topic, payload) = router::published(message)?;
    if !from_logind(services, message.sender) {
        return None;
    }
    if topic == logind::wire::TOPIC_SYSTEM_EVENTS_LOGIN_END {
        let end = logind::wire::decode_login_end(&payload).ok()?;
        forget(end.session);
        return (end.session != 0).then_some(end.session);
    }
    // `system/events/login/session/<id>`: the id is the last segment.
    let prefix = logind::wire::TOPIC_SYSTEM_EVENTS_LOGIN_SESSION.trim_end_matches('+');
    let id = topic
        .strip_prefix(prefix)
        .and_then(|id| id.parse::<u64>().ok())?;
    let record = logind::wire::decode_login_session(&payload).ok()?;
    // Only a live session has an owner; any other state (exited, or one this
    // table does not know) forgets it, so no stale owner outlives its session.
    if matches!(record.state.as_str(), "starting" | "active") {
        remember(
            id,
            Owner {
                uid: record.uid,
                user: record.user,
                home: record.home,
            },
        );
    } else {
        forget(id);
    }
    None
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

/// Record `session` as owned by `owner`, reusing its slot, a free one, or
/// (when the table is full) the oldest.
fn remember(session: u64, owner: Owner) {
    if session == 0 {
        return;
    }
    let mut table = TABLE.lock();
    if let Some(slot) = table.iter_mut().find(|slot| slot.session == session) {
        slot.owner = owner;
    } else if table.len() < SLOTS {
        table.push(Slot { session, owner });
    } else {
        let victim = (NEXT.fetch_add(1, Ordering::Relaxed) as usize) % SLOTS;
        table[victim] = Slot { session, owner };
    }
}

/// Drop `session` (it ended).
fn forget(session: u64) {
    TABLE.lock().retain(|slot| slot.session != session);
}

/// The self-test: an event from a task that is not `logind` is ignored, one
/// from `logind` is remembered (with the account its environment comes from)
/// and an end forgets it again. Exercised through the table directly (a
/// synthetic `Message` would need a real sender). Prints `INIT:SESSIONS:PASS`.
pub(super) fn selftest() -> &'static str {
    const PROBE: u64 = 0xfeed_0001;
    let home = fhs::home_of("probe");
    let probe = |uid: u32| Owner {
        uid,
        user: String::from("probe"),
        home: home.clone(),
    };
    let before = owner(PROBE).is_none();
    remember(PROBE, probe(4321));
    let learned = owner(PROBE) == Some(4321);
    remember(PROBE, probe(1234));
    let updated = owner(PROBE) == Some(1234);
    let want_home = alloc::format!("HOME={home}");
    let environment = env(PROBE, 1234).contains(&want_home)
        && env(PROBE, 1234).iter().any(|var| var == "USER=probe");
    forget(PROBE);
    let forgotten = owner(PROBE).is_none() && owner(0).is_none();
    let refused = !from_logind(&[], 1);
    if before && learned && updated && environment && forgotten && refused {
        "INIT:SESSIONS:PASS\n"
    } else {
        "INIT:SESSIONS:FAIL session owner table broke\n"
    }
}
