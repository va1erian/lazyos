//! The app failures `init` publishes (issue #549), as the shell hears them.
//!
//! `init` publishes `system/events/app/<id>` on the central broker
//! (`messengerd`) when it gives up on a launched app. The shell subscribes
//! once the broker is there (a buffered subscription, so failures that land
//! between two looks wait); the watcher thread (`fabric::Follow`) reads it
//! off the broker's doorbell, and the heartbeat only empties a queue, so the
//! shell never waits on the broker. Only failures of apps in this shell's own
//! login session are kept.
//! The topic is retained per app, so a shell that starts after an app failed
//! at boot still hears of it; a retained failure older than [`STALE_TICKS`]
//! (an old one replayed to a restarted shell) is skipped, and each failure is
//! shown once (`(app, at)`).
//! The payload comes out of the broker's envelope (`libmessenger::envelope`)
//! before it gets here.
//!
//! Serial: `SHELL:FAILURES:SUBSCRIBED` once, `SHELL:FAILURE app=<id>
//! status=<n>` per failure kept.

use lazyshell::notice::Failure;
use messenger_generated::os_lazy_init_v1 as init_wire;

use crate::fabric::{pull, Feed, Watch};
use crate::sys::{self};
use messenger_generated::topics::Event;

/// A retained failure older than this (one minute) is history, not news.
const STALE_TICKS: u64 = 6000;
/// Failures remembered to show each once.
const SEEN: usize = 32;

/// Follows `system/events/app/+` for one session.
pub struct FailureFeed {
    /// This shell's login session; `None` when unknown (then nothing shows:
    /// a notice for another user's app would leak what they run).
    session: Option<u64>,
    /// The watcher thread's follow of the topic (a buffered subscription, so
    /// failures that land between two looks wait); none without a session.
    follow: Option<Feed<init_wire::AppFailure>>,
    /// Whether the subscription was announced on the serial line.
    announced: bool,
    /// `(app, at)` of the failures already handed out, newest last.
    seen: Vec<(String, u64)>,
}

impl FailureFeed {
    pub fn new(session: Option<u64>) -> FailureFeed {
        FailureFeed {
            session,
            follow: session
                .and_then(|_| init_wire::subscribe_system_events_app(&mut Watch, "+").ok()),
            announced: false,
            seen: Vec::new(),
        }
    }

    /// The failures of this session's apps that arrived since the last look.
    /// Reads a queue in memory: the watcher thread does the broker's calls.
    pub fn poll(&mut self) -> Vec<Failure> {
        let Some(stream) = self.follow.as_mut() else {
            return Vec::new();
        };
        if !self.announced && stream.handle().subscribed() {
            self.announced = true;
            println!("SHELL:FAILURES:SUBSCRIBED");
        }
        let mut events = Vec::new();
        pull(stream, |event| events.push(event));
        events
            .into_iter()
            .filter_map(|event| self.keep(event))
            .collect()
    }

    /// The failure in `event`, when it is one of this session's apps.
    fn keep(&mut self, event: Event<init_wire::AppFailure>) -> Option<Failure> {
        let app = event.topic.rsplit('/').next()?.to_owned();
        let failure = event.value;
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
