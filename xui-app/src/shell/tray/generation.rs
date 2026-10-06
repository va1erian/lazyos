//! The tray's generation (docs/tray-plan.md section 4, "Shell restart").
//!
//! Items live in the shell, so a restarted shell starts empty. Once it serves
//! `os.lazy.shell.tray` it publishes a new value on the retained
//! `session/<s>/shell/tray` topic; every client library follows that topic
//! and calls `Set` again when the value changes. Before publishing, the shell
//! reads the value a previous shell left: when there was one, this is a
//! restart, and each item set in the next [`RESTORE_TICKS`] is reported as
//! `SHELL:TRAY:RESTORED n=<items>`.

use messenger_generated::os_lazy_messenger_topics_v1 as topics;
use messenger_generated::os_lazy_shell_tray_v1 as wire;

use super::super::ctx::Ctx;
use super::TrayState;
use crate::platform::topic_feed::TopicFeed;
use crate::platform::topics::Broker;
use crate::sys;

/// How long the shell waits for the previous generation's retained value.
const READ_TICKS: u64 = 50;
/// How long after a restart a `Set` counts as an item coming back (30 s).
const RESTORE_TICKS: u64 = 3000;

/// This shell's generation.
pub struct Generation {
    session: Option<u64>,
    feed: Option<TopicFeed>,
    /// When restored items are still being reported (a PIT tick).
    restore_until: Option<u64>,
}

impl Generation {
    pub fn new(session: Option<u64>) -> Generation {
        let feed = session
            .and_then(|session| wire::name_session_shell_tray(&session.to_string()).ok())
            .map(|topic| TopicFeed::new(topic, topics::QOS_LATEST, 1, u64::MAX));
        Generation {
            session,
            feed,
            restore_until: None,
        }
    }

    /// The shell's login session, when known.
    pub fn session(&self) -> Option<u64> {
        self.session
    }

    /// The service just started answering `Set`: read what a previous shell
    /// published, then publish a new generation (the boot tick: it grows
    /// across restarts, so it never repeats).
    pub fn serving(&mut self, ctx: &Ctx) {
        let Some(session) = self.session else {
            return;
        };
        let previous = self
            .feed
            .as_mut()
            .map(|feed| feed.poll_waiting(READ_TICKS))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|event| wire::decode_session_shell_tray(&event.payload).ok())
            .next_back();
        let now = sys::clock_ticks();
        let generation = previous
            .as_ref()
            .map_or(now, |old| now.max(old.generation.saturating_add(1)));
        if previous.is_some() {
            self.restore_until = Some(now.saturating_add(RESTORE_TICKS));
        }
        let value = wire::Generation { generation };
        match wire::publish_session_shell_tray(&mut Broker::new(), &session.to_string(), &value) {
            Ok(_) => println!(
                "SHELL:TRAY:GENERATION n={generation} restart={}",
                previous.is_some()
            ),
            Err(error) => ctx.note("tray-generation", || {
                format!("SHELL:TRAY:GENERATION:FAIL {error:?}")
            }),
        }
    }

    /// An item was just set: after a restart, report it as restored.
    pub fn restored(&self, tray: &TrayState) {
        let within = self
            .restore_until
            .is_some_and(|until| sys::clock_ticks() <= until);
        if within {
            let count = tray
                .model
                .borrow()
                .entries()
                .iter()
                .filter(|entry| entry.custom.is_some())
                .count();
            println!("SHELL:TRAY:RESTORED n={count}");
        }
    }
}
