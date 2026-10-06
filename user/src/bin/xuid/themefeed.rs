//! Live theme: follows the `sys/ui/*` settings in `confd` (issue: Settings app),
//! overlaid with the desktop user's own `user/<uid>/ui/*` (issue #407).
//! The clock format (`sys/time/*`) belongs to the LazyShell taskbar (#157).
//!
//! On the first successful poll the feed reads every key, then subscribes to
//! `system/confd/changed/sys/ui/#`; any change event triggers a re-read
//! (the topic carries only the path, never the value). The resolved palette is
//! installed into [`theme`](super::theme) and the caller repaints when it
//! changed. Everything is bounded and retried: `xuid` must never stall or
//! fail because `confd` or the broker is late, in which case the compiled-in
//! defaults simply stay in effect.
//!
//! The user is whoever runs the shell ([`ThemeFeed::follow_user`], from the
//! shell's `Subscribe`): a graphical login's LazyShell runs as that user.
//! For a user other than the administrator, each of its `user/<uid>/ui/*`
//! keys shadows the machine key, and the feed also follows
//! `user/<uid>/confd/changed/ui/#`, which only that uid and root (`xuid`) may
//! subscribe to.
//!
//! Re-reads allocate (the user bump allocator never reclaims), but they happen
//! only when a setting actually changes.

use alloc::string::String;
use alloc::vec::Vec;
use messenger_generated::topics;
use uitheme::Settings;
use user::central::{Bus, Subscription};
use user::messenger::confd::{wire, Client};
use user::messenger::topics_client::Qos;
use user::messenger::{DEFAULT_BUFFER, EXPIRED_DEADLINE};
use user::sys;

use super::theme;

/// Ticks (100 Hz) between looks at the change topic.
const POLL_TICKS: u64 = 25;
/// Ticks between attempts to reach `confd` or the broker while unreachable.
const RETRY_TICKS: u64 = 300;
/// Ticks the subscribe call may wait for the broker before it is abandoned.
const SUBSCRIBE_TICKS: u64 = 5;
/// Topic filter for every `sys/ui/*` change.
const FILTER: &str = "system/confd/changed/sys/ui/#";

pub(super) struct ThemeFeed {
    /// Held for the life of the compositor: closing a resolved handle would
    /// take the service down with it.
    client: Option<Client>,
    watch: Option<Subscription>,
    /// The shell's uid when it has a personal theme ([`uitheme::personal`]).
    user: Option<u32>,
    /// The change subscription for `user`'s own theme keys.
    user_watch: Option<Subscription>,
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
            user: None,
            user_watch: None,
            next_poll: 0,
            next_connect: 0,
            settings,
            buffer: alloc::vec![0u8; DEFAULT_BUFFER],
        }
    }

    /// Decide the UI scale for this compositor's lifetime (docs/hidpi-plan.md):
    /// `sys/ui/scale` when `confd` already answers, else `auto`, resolved
    /// against a `width x height` screen. One attempt only: the scale must be
    /// fixed before the first client asks for it, and a late `confd` simply
    /// leaves the automatic choice. The client stays held for the feed.
    pub(super) fn decide_scale(&mut self, width: u32, height: u32) -> u32 {
        if self.client.is_none() {
            self.client = Client::connect().ok();
        }
        let setting = match &self.client {
            Some(client) => match client.get(uitheme::KEY_SCALE) {
                Ok(value) => uitheme::UiScale::from_value(value.as_ref()),
                Err(_) => uitheme::UiScale::Auto,
            },
            None => uitheme::UiScale::Auto,
        };
        let scale = setting.resolve(width, height);
        theme::set_scale(scale);
        sys::write_str(&alloc::format!(
            "XUID:SCALE:{scale} setting={} screen={width}x{height}\n",
            setting.as_str()
        ));
        scale
    }

    /// Whether the desktop animations are enabled (`sys/ui/anim`).
    pub(super) fn animations(&self) -> bool {
        self.settings.anim
    }

    /// Paint for the shell's user `uid` from now on; `true` when the palette
    /// changed and the screen needs a full repaint. The administrator (and
    /// the boot-time shell, uid 0) paints from the machine keys alone.
    pub(super) fn follow_user(&mut self, uid: u32) -> bool {
        let user = uitheme::personal(uid).then_some(uid);
        if user == self.user {
            return false;
        }
        self.user = user;
        if let Some(watch) = self.user_watch.take() {
            let _ = watch.unsubscribe();
        }
        sys::write_str(&alloc::format!("THEME:USER uid={uid}\n"));
        if let Some(uid) = user {
            self.user_watch = self.subscribe_user(uid);
        }
        self.reload()
    }

    /// Follow confd; `true` when the palette changed and the screen needs a
    /// full repaint.
    pub(super) fn poll(&mut self) -> bool {
        let now = sys::clock();
        if now < self.next_poll {
            return false;
        }
        self.next_poll = now + POLL_TICKS;
        let user_missing = self.user.is_some() && self.user_watch.is_none();
        if self.client.is_none() || self.watch.is_none() || user_missing {
            if now < self.next_connect {
                return false;
            }
            self.next_connect = now + RETRY_TICKS;
            return self.connect(now);
        }
        let machine = drain(&mut self.watch, &mut self.buffer);
        let own = drain(&mut self.user_watch, &mut self.buffer);
        if machine || own {
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
            self.watch = subscribe(String::from(FILTER), now);
        }
        if let (Some(uid), None) = (self.user, &self.user_watch) {
            self.user_watch = self.subscribe_user(uid);
        }
        // Read even without a subscription so settings written before the
        // broker came up still apply; the next retry subscribes.
        self.reload()
    }

    /// Subscribe to `uid`'s own theme changes (`user/<uid>/confd/changed/ui/#`).
    fn subscribe_user(&self, uid: u32) -> Option<Subscription> {
        let uid = alloc::format!("{uid}");
        let filter = topics::build(
            wire::TOPIC_USER_CONFD_CHANGED,
            &[&uid, uitheme::USER_FILTER_PATH],
            topics::Mode::Subscribe,
        )
        .ok()?;
        subscribe(filter, sys::clock())
    }

    /// Re-read the theme keys and install the resulting palette; `true` when
    /// it changed what is on screen.
    fn reload(&mut self) -> bool {
        let Some(client) = &self.client else {
            return false;
        };
        let mut settings = Settings::default();
        for key in uitheme::ALL_KEYS {
            let Ok(machine) = client.get(key) else {
                // A failing service keeps the previous theme.
                return false;
            };
            let own = match self.user.and_then(|uid| uitheme::user_key(uid, key)) {
                Some(path) => match client.get(&path) {
                    Ok(value) => value,
                    Err(_) => return false,
                },
                None => None,
            };
            settings.apply(key, uitheme::overlay(machine, own).as_ref());
        }
        self.settings = settings;
        let changed = theme::set_palette(&uitheme::resolve(&settings));
        if changed {
            sys::write_str("THEME:APPLIED\n");
        }
        changed
    }
}

/// Subscribe to `filter`, bounded: the compositor must not stall on a silent
/// broker. A failure is logged and retried by a later poll.
fn subscribe(filter: String, now: u64) -> Option<Subscription> {
    let deadline = Some(now + SUBSCRIBE_TICKS);
    let watch = Bus::connect()
        .and_then(|mut bus| bus.subscribe_with_deadline(&filter, Qos::Latest, deadline));
    if let Err(error) = &watch {
        sys::write_str(&alloc::format!("THEME:WATCH:FAIL {filter} {error:?}\n"));
    }
    watch.ok()
}

/// Whether at least one change event arrived on `watch` (all pending ones
/// consumed). A broken subscription is dropped, so a later poll resubscribes.
fn drain(watch: &mut Option<Subscription>, buffer: &mut [u8]) -> bool {
    let Some(subscription) = watch else {
        return false;
    };
    let mut any = false;
    loop {
        // A poll (`EXPIRED_DEADLINE`), never a wait: the broker parks a
        // `NextEvent` with no event until the caller's deadline, and a
        // two-tick deadline here froze the compositor (and the cursor)
        // for 10 to 20 ms every `POLL_TICKS`. The kernel keeps a poll open
        // for the broker's whole service turn (`channels::POLL_DEADLINE`),
        // so a queued event is still delivered.
        match subscription.recv_with(buffer, Some(EXPIRED_DEADLINE)) {
            Ok(Some(_)) => any = true,
            Ok(None) => break,
            Err(_) => {
                *watch = None;
                break;
            }
        }
    }
    any
}
