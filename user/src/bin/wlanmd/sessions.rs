//! Who is logged in, for user-scope auto-connect
//! (`wlansta::Station::set_active_users`): a user's networks are joined on
//! their own only while that user has a session.
//!
//! `logind` announces sessions on `init`'s broker
//! (`system/events/login/start`, `.../end`, `.../session/<id>`), but the
//! broker takes a publish from anyone, so an event is only a **hint**: on
//! each one (and every [`POLL_TICKS`] in case one was missed) `wlanmd` asks
//! `logind` itself (`Sessions`, through the registry name only `logind`
//! holds) and believes that answer. A console `logind` sitting at the login
//! prompt answers only after the next key, so the question has a short
//! deadline; when it goes unanswered after an `end` hint, the user named in
//! it is dropped anyway (dropping only ever stops auto-connect, so a forged
//! `end` costs nothing worse than a manual `Connect`), and nothing is ever
//! added on a hint alone.
//!
//! Serial: `WLANMD:USERS <uid,...>` when the set changes.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use user::trace;

use user::messenger::{logind, registry, router, services, DEFAULT_BUFFER, EXPIRED_DEADLINE};
use user::sys;

/// Ticks (100 Hz) between attempts to subscribe while `init` is unreachable.
const RETRY_TICKS: u64 = 1_000;
/// Ticks between questions to `logind` when no hint came.
const POLL_TICKS: u64 = 3_000;
/// How long `logind` may take to answer (a console `logind` at its prompt
/// does not answer at all).
const ANSWER_TICKS: u64 = 10;
/// Events read per look.
const MAX_PER_LOOK: usize = 32;
/// The session states of a live session (`elevpolicy::sessions`).
const LIVE: [&str; 2] = ["starting", "active"];

pub(crate) struct Logins {
    subs: Vec<router::Subscriber>,
    next_connect: u64,
    next_poll: u64,
    buffer: Vec<u8>,
    /// The users last believed logged in.
    users: Vec<u32>,
}

impl Logins {
    pub(crate) fn new() -> Logins {
        Logins {
            subs: Vec::new(),
            next_connect: 0,
            next_poll: 0,
            buffer: alloc::vec![0u8; DEFAULT_BUFFER],
            users: Vec::new(),
        }
    }

    /// The users last believed logged in.
    pub(crate) fn users(&self) -> &[u32] {
        &self.users
    }

    /// The logged-in users, when they changed since the last look.
    pub(crate) fn changed(&mut self) -> Option<Vec<u32>> {
        let now = sys::clock();
        self.subscribe(now);
        let (hinted, ended) = self.hints();
        if !hinted && now < self.next_poll {
            return None;
        }
        self.next_poll = now + POLL_TICKS;
        let users = match ask_logind(&mut self.buffer) {
            Some(users) => users,
            None if !ended.is_empty() => {
                let mut users = self.users.clone();
                users.retain(|uid| !ended.contains(uid));
                users
            }
            None => return None,
        };
        if users == self.users {
            return None;
        }
        trace::line!(trace::Channel::Wlanmd, "USERS {}", list(&users));
        self.users = users.clone();
        Some(users)
    }

    fn subscribe(&mut self, now: u64) {
        if !self.subs.is_empty() || now < self.next_connect {
            return;
        }
        self.next_connect = now + RETRY_TICKS;
        let Ok(mut bus) = router::Bus::connect(services::INIT_NAME) else {
            return;
        };
        let subs = [
            logind::wire::subscribe_system_events_login_start(&mut bus),
            logind::wire::subscribe_system_events_login_end(&mut bus),
            logind::wire::subscribe_system_events_login_session(&mut bus, "+"),
        ];
        self.subs = subs.into_iter().filter_map(Result::ok).collect();
    }

    /// Drain the hints: whether any came, and the uids an `end` (or a
    /// session record that is not live) named.
    fn hints(&mut self) -> (bool, Vec<u32>) {
        let (mut hinted, mut ended) = (false, Vec::new());
        let mut broken = false;
        for sub in &self.subs {
            for _ in 0..MAX_PER_LOOK {
                match sub.recv_with(&mut self.buffer, Some(EXPIRED_DEADLINE)) {
                    Ok(Some(event)) => {
                        hinted = true;
                        if let Some(uid) = ended_uid(&event) {
                            ended.push(uid);
                        }
                    }
                    Ok(None) => break,
                    Err(_) => {
                        broken = true;
                        break;
                    }
                }
            }
        }
        if broken {
            // The broker went away: subscribe again later (and replay).
            self.subs.clear();
        }
        (hinted, ended)
    }
}

/// The uid an event says left, if it says one did.
fn ended_uid(event: &router::Event) -> Option<u32> {
    if event.topic == logind::wire::TOPIC_SYSTEM_EVENTS_LOGIN_END {
        return logind::wire::decode_login_end(&event.payload)
            .ok()
            .map(|end| end.uid);
    }
    let record = logind::wire::decode_login_session(&event.payload).ok()?;
    (!LIVE.contains(&record.state.as_str())).then_some(record.uid)
}

/// `logind`'s own answer: the uids of the live sessions, or `None` when it
/// did not answer in time. `buf` is the reused receive buffer (the user
/// heap never reclaims a per-call one).
fn ask_logind(buf: &mut [u8]) -> Option<Vec<u32>> {
    let endpoint = registry::resolve(logind::NAME).ok()?;
    let deadline = Some(sys::clock() + ANSWER_TICKS);
    let reply = endpoint.call_with(&logind::sessions_request(), buf, deadline);
    let _ = endpoint.release();
    let (_, sessions) = logind::decode_sessions(&reply.ok()?).ok()?;
    let mut users: Vec<u32> = sessions
        .iter()
        .filter(|s| LIVE.contains(&s.state.as_str()))
        .map(|s| s.uid)
        .collect();
    users.sort_unstable();
    users.dedup();
    Some(users)
}

fn list(users: &[u32]) -> String {
    let names: Vec<String> = users.iter().map(|uid| format!("{uid}")).collect();
    names.join(",")
}
