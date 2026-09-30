//! Live theme: follows the `sys/ui/*` settings in `confd` (issue: Settings app).
//!
//! On the first successful poll the feed reads every key, then subscribes to
//! `system/confd/changed/sys/ui/#`; any change event triggers a re-read
//! (the topic carries only the path, never the value). The resolved palette is
//! installed into [`theme`](super::theme) and the caller repaints when it
//! changed. Everything is bounded and retried: `xuid` must never stall or
//! fail because `confd` or the broker is late, in which case the compiled-in
//! defaults simply stay in effect.
//!
//! Re-reads allocate (the user bump allocator never reclaims), but they happen
//! only when a setting actually changes.

use alloc::vec::Vec;
use uitheme::Settings;
use user::central::{Bus, Subscription};
use user::messenger::confd::Client;
use user::messenger::topics_client::Qos;
use user::messenger::DEFAULT_BUFFER;
use user::sys;

use super::theme;

/// Ticks (100 Hz) between looks at the change topic.
const POLL_TICKS: u64 = 25;
/// Ticks between attempts to reach `confd` or the broker while unreachable.
const RETRY_TICKS: u64 = 300;
/// Ticks one topic poll may wait: an already-expired deadline makes the
/// caller leave before the broker answers, which loses the event.
const RECV_TICKS: u64 = 2;
/// Ticks the subscribe call may wait for the broker before it is abandoned.
const SUBSCRIBE_TICKS: u64 = 5;
/// Topic filter for every `sys/ui/*` change.
const FILTER: &str = "system/confd/changed/sys/ui/#";

pub(super) struct ThemeFeed {
    /// Held for the life of the compositor: closing a resolved handle would
    /// take the service down with it.
    client: Option<Client>,
    watch: Option<Subscription>,
    next_poll: u64,
    next_connect: u64,
    /// The settings currently applied, for change detection.
    settings: Settings,
    /// Reused reply buffer for the topic poll.
    buffer: Vec<u8>,
}

impl ThemeFeed {
    /// Install the default palette and start with nothing connected.
    pub(super) fn new() -> ThemeFeed {
        let settings = Settings::default();
        theme::set_palette(&uitheme::resolve(&settings));
        ThemeFeed {
            client: None,
            watch: None,
            next_poll: 0,
            next_connect: 0,
            settings,
            buffer: alloc::vec![0u8; DEFAULT_BUFFER],
        }
    }

    /// Whether the desktop animations are enabled.
    #[allow(dead_code)]
    pub(super) fn animations(&self) -> bool {
        self.settings.anim
    }

    /// Follow confd; `true` when the palette changed and the screen needs a
    /// full repaint.
    pub(super) fn poll(&mut self) -> bool {
        let now = sys::clock();
        if now < self.next_poll {
            return false;
        }
        self.next_poll = now + POLL_TICKS;
        if self.client.is_none() || self.watch.is_none() {
            if now < self.next_connect {
                return false;
            }
            self.next_connect = now + RETRY_TICKS;
            return self.connect(now);
        }
        if self.drain_events() {
            return self.reload();
        }
        false
    }

    /// Connect to confd and the broker, then read the current values.
    fn connect(&mut self, now: u64) -> bool {
        if self.client.is_none() {
            // Expected to fail until `confd` registers; retried quietly.
            self.client = Client::connect().ok();
        }
        if self.client.is_none() {
            return false;
        }
        if self.watch.is_none() {
            // Bounded: the compositor must not stall on a silent broker.
            let deadline = Some(now + SUBSCRIBE_TICKS);
            let watch = Bus::connect()
                .and_then(|mut bus| bus.subscribe_with_deadline(FILTER, Qos::Latest, deadline));
            if let Err(error) = &watch {
                sys::write_str(&alloc::format!(
                    "THEME:WATCH:FAIL {error:?}
"
                ));
            }
            self.watch = watch.ok();
        }
        // Read even without a subscription so settings written before the
        // broker came up still apply; the next retry subscribes.
        self.reload()
    }

    /// Whether at least one change event arrived (all pending ones consumed).
    fn drain_events(&mut self) -> bool {
        let Some(watch) = &self.watch else {
            return false;
        };
        let mut any = false;
        loop {
            match watch.recv_with(&mut self.buffer, Some(sys::clock() + RECV_TICKS)) {
                Ok(Some(_)) => any = true,
                Ok(None) => break,
                Err(_) => {
                    // The broker went away: resubscribe on a later poll.
                    self.watch = None;
                    break;
                }
            }
        }
        any
    }

    /// Re-read every key and install the resulting palette.
    fn reload(&mut self) -> bool {
        let Some(client) = &self.client else {
            return false;
        };
        let mut settings = Settings::default();
        for key in uitheme::ALL_KEYS {
            match client.get(key) {
                Ok(value) => settings.apply(key, value.as_ref()),
                // A failing service keeps the previous theme.
                Err(_) => return false,
            }
        }
        self.settings = settings;
        let changed = theme::set_palette(&uitheme::resolve(&settings));
        if changed {
            sys::write_str(
                "THEME:APPLIED
",
            );
        }
        changed
    }
}
