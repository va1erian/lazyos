//! Who is logged in, for the services that act on the user's behalf only
//! while they have a session: `wlanmd`'s user-scope auto-connect
//! (`wlansta::Station::set_active_users`) and `mountd`'s automount of
//! removable volumes, whose files belong to the session user.
//!
//! `logind` announces sessions on the central broker
//! (`system/events/login/start`, `.../end`, `.../session/<id>`), read through
//! one [`Feed`] each (a doorbell asked locally, never the broker, on every
//! look), but the
//! broker takes a publish from anyone, so an event is only a **hint**: on
//! each one (and every [`POLL_TICKS`] in case one was missed) `wlanmd` asks
//! `logind` itself (`Sessions`, through the registry name only `logind`
//! holds) and believes that answer. A console `logind` sitting at the login
//! prompt answers only after the next key, so the question has a short
//! deadline; when it goes unanswered after an `end` hint, the user named in
//! it is dropped anyway (for `wlanmd` dropping only stops auto-connect; for
//! `mountd` it unmounts that user's volumes, which a forged `end` could
//! cause until the next answer from `logind` puts the session back: a
//! nuisance, never a way in), and nothing is ever added on a hint alone.
//!
//! Serial: `<CHANNEL>:USERS <uid,...>` when the set changes, on the channel
//! of the service that asked.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::client::Deadline;

use crate::central::Feed;
use crate::messenger::client::Native;
use crate::messenger::logind;
use crate::{sys, trace};

/// Ticks between questions to `logind` when no hint came.
const POLL_TICKS: u64 = 3_000;
/// How long `logind` may take to answer (a console `logind` at its prompt
/// does not answer at all).
const ANSWER_TICKS: u64 = 10;
/// The session states of a live session (`elevpolicy::sessions`).
const LIVE: [&str; 2] = ["starting", "active"];

pub struct Logins {
    channel: trace::Channel,
    /// The start, end and session feeds; one the broker refused (it was full,
    /// or briefly unreachable) is made again.
    start: Feed<logind::wire::LoginStart>,
    end: Feed<logind::wire::LoginEnd>,
    session: Feed<logind::wire::LoginSession>,
    next_poll: u64,
    /// The users last believed logged in.
    users: Vec<u32>,
}

impl Logins {
    /// A follower that reports changes on `channel`.
    pub fn new(channel: trace::Channel) -> Logins {
        Logins {
            channel,
            start: Feed::new(logind::wire::subscribe_system_events_login_start),
            end: Feed::new(logind::wire::subscribe_system_events_login_end),
            session: Feed::new(|bus| logind::wire::subscribe_system_events_login_session(bus, "+")),
            next_poll: 0,
            users: Vec::new(),
        }
    }

    /// The users last believed logged in.
    pub fn users(&self) -> &[u32] {
        &self.users
    }

    /// The logged-in users, when they changed since the last look.
    pub fn changed(&mut self) -> Option<Vec<u32>> {
        let now = sys::clock();
        self.start.ensure();
        self.end.ensure();
        self.session.ensure();
        let (hinted, ended) = self.hints();
        if !hinted && now < self.next_poll {
            return None;
        }
        self.next_poll = now + POLL_TICKS;
        let users = match ask_logind() {
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
        trace::line!(self.channel, "USERS {}", list(&users));
        self.users = users.clone();
        Some(users)
    }

    /// Drain the hints: whether any came, and the uids an `end` (or a
    /// session record that is not live) named.
    fn hints(&mut self) -> (bool, Vec<u32>) {
        let (mut hinted, mut ended) = (false, Vec::new());
        self.start.drain_rung(|_| hinted = true);
        self.end.drain_rung(|event| {
            hinted = true;
            ended.push(event.value.uid);
        });
        self.session.drain_rung(|event| {
            hinted = true;
            if !LIVE.contains(&event.value.state.as_str()) {
                ended.push(event.value.uid);
            }
        });
        (hinted, ended)
    }
}

/// `logind`'s own answer: the uids of the live sessions, or `None` when it
/// did not answer in time.
fn ask_logind() -> Option<Vec<u32>> {
    let client = logind::Client::try_connect(Native).ok()?;
    let reply = client.sessions_within(Deadline::ticks(ANSWER_TICKS)).ok()?;
    let mut users: Vec<u32> = reply
        .sessions
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
