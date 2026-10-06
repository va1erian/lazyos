//! The app failures `init` publishes (issue #549), as the shell hears them.
//!
//! `init` publishes `system/events/app/<id>` on the central broker
//! (`messengerd`) when it gives up on a launched app. The shell subscribes
//! once the broker is there (a buffered subscription, so failures that land
//! between two looks wait) and pulls with poll calls on the heartbeat: the
//! kernel answers a poll in the broker's own service turn, so the shell never
//! parks. Only failures of apps in this shell's own login session are kept.
//! The topic is retained per app, so a shell that starts after an app failed
//! at boot still hears of it; a retained failure older than [`STALE_TICKS`]
//! (an old one replayed to a restarted shell) is skipped, and each failure is
//! shown once (`(app, at)`).
//! Every call uses the generated `os.lazy.messenger.topics.v1` and
//! `os.lazy.init.v1` stubs; the payload comes out of the broker's envelope
//! (`libmessenger::envelope`).
//!
//! Serial: `SHELL:FAILURES:SUBSCRIBED` once, `SHELL:FAILURE app=<id>
//! status=<n>` per failure kept.

use lazyshell::notice::Failure;
use messenger_generated::os_lazy_init_v1 as init_wire;
use messenger_generated::os_lazy_messenger_topics_v1 as topics;

use crate::platform::messenger::Service;
use crate::server::ERROR_FIELD;
use crate::sys::{self, EXPIRED_DEADLINE};

/// The central broker's registered name.
const BROKER: &str = "os.lazy.messenger.topics";
/// Failures the broker may hold for the shell between two looks.
const DEPTH: u32 = 16;
/// Ticks between looks (100 Hz).
const POLL_TICKS: u64 = 25;
/// Ticks between subscription attempts while the broker is away.
const RETRY_TICKS: u64 = 200;
/// Most events taken per look.
const PER_LOOK: usize = 8;
/// Ticks the subscribe call may take.
const SUBSCRIBE_TICKS: u64 = 50;
/// A retained failure older than this (one minute) is history, not news.
const STALE_TICKS: u64 = 6000;
/// Failures remembered to show each once.
const SEEN: usize = 32;

/// Follows `system/events/app/+` for one session.
pub struct FailureFeed {
    /// This shell's login session; `None` when unknown (then nothing shows:
    /// a notice for another user's app would leak what they run).
    session: Option<u64>,
    subscription: Option<u64>,
    next_look: u64,
    /// `(app, at)` of the failures already handed out, newest last.
    seen: Vec<(String, u64)>,
}

impl FailureFeed {
    pub fn new(session: Option<u64>) -> FailureFeed {
        FailureFeed {
            session,
            subscription: None,
            next_look: 0,
            seen: Vec::new(),
        }
    }

    /// The failures of this session's apps that arrived since the last look.
    pub fn poll(&mut self) -> Vec<Failure> {
        let now = sys::clock_ticks();
        if now < self.next_look || self.session.is_none() {
            return Vec::new();
        }
        self.next_look = now.saturating_add(POLL_TICKS);
        let Some(subscription) = self.subscription.or_else(|| self.subscribe(now)) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for _ in 0..PER_LOOK {
            match next_event(subscription) {
                Ok(Some(event)) => found.extend(self.keep(event)),
                Ok(None) => break,
                // The broker restarted (or refused): subscribe again later.
                Err(_) => {
                    self.subscription = None;
                    break;
                }
            }
        }
        found
    }

    fn subscribe(&mut self, now: u64) -> Option<u64> {
        match subscribe() {
            Ok(id) => {
                println!("SHELL:FAILURES:SUBSCRIBED");
                self.subscription = Some(id);
                Some(id)
            }
            Err(_) => {
                self.next_look = now.saturating_add(RETRY_TICKS);
                None
            }
        }
    }

    /// The failure in `event`, when it is one of this session's apps.
    fn keep(&mut self, event: topics::Event) -> Option<Failure> {
        let app = event.topic.rsplit('/').next()?.to_owned();
        let payload = libmessenger::envelope::unwrap(&event.payload);
        let failure = init_wire::decode_system_events_app(&payload).ok()?;
        if Some(failure.session) != self.session {
            return None;
        }
        let stale = sys::clock_ticks().saturating_sub(failure.at) > STALE_TICKS;
        let key = (app.clone(), failure.at);
        if (event.retained && stale) || self.seen.contains(&key) {
            return None;
        }
        if self.seen.len() >= SEEN {
            self.seen.remove(0);
        }
        self.seen.push(key);
        println!("SHELL:FAILURE app={app} status={}", failure.status);
        Some(Failure {
            app,
            name: failure.name,
            summary: failure.summary,
            reason: failure.reason,
            startup: failure.startup,
        })
    }
}

fn broker() -> Result<Service, i64> {
    Service::try_connect(BROKER).ok_or(-sys::errno::ENOENT)
}

/// `Subscribe("system/events/app/+", Buffered, DEPTH)`: the subscription id.
fn subscribe() -> Result<u64, i64> {
    let body = topics::encode_subscribe_args(&topics::SubscribeArgs {
        filter: init_wire::TOPIC_SYSTEM_EVENTS_APP.to_owned(),
        qos: topics::QOS_BUFFERED,
        depth: DEPTH,
    })
    .map_err(|_| -sys::errno::EINVAL)?;
    let reply = broker()?.call_within(
        topics::INTERFACE_ID,
        topics::METHOD_SUBSCRIBE,
        ERROR_FIELD,
        body,
        SUBSCRIBE_TICKS,
    )?;
    topics::decode_subscribe_reply(&reply.body)
        .map(|reply| reply.subscription)
        .map_err(|_| -sys::errno::EINVAL)
}

/// One poll `NextEvent`: `None` when nothing is queued.
fn next_event(subscription: u64) -> Result<Option<topics::Event>, i64> {
    let body = topics::encode_next_event_args(&topics::NextEventArgs { subscription })
        .map_err(|_| -sys::errno::EINVAL)?;
    let reply = match broker()?.call_until(
        topics::INTERFACE_ID,
        topics::METHOD_NEXTEVENT,
        ERROR_FIELD,
        body,
        EXPIRED_DEADLINE,
    ) {
        Ok(reply) => reply,
        Err(code) if code == -sys::errno::ETIMEDOUT => return Ok(None),
        Err(code) => return Err(code),
    };
    let event = topics::decode_next_event_reply(&reply.body)
        .map_err(|_| -sys::errno::EINVAL)?
        .event;
    Ok((!event.topic.is_empty()).then_some(event))
}
