//! The logged-in user's own keyboard layout (`user/<uid>/input/layout`,
//! `inputmap::session_layout`), for `inputd`.
//!
//! `inputd` may not read a user's `confd` keys and does not know who is
//! logged in; the compositor knows both (the shell's uid, like the theme in
//! `themefeed.rs`). This feed reads the key, follows its changes on
//! `user/<uid>/confd/changed/input/#`, and [`LayoutFeed::wanted`] is what
//! `inputlink.rs` hands `inputd` with `NoteSessionLayout`. `None` means the
//! machine default: no shell yet, uid 0, a user without a choice of its own,
//! a value no layout has, or a logout (the login screen types with the
//! machine's layout). Bounded and retried like the theme feed: a late `confd`
//! or broker only leaves the machine default in effect.

use alloc::format;
use alloc::vec::Vec;

use inputmap::session_layout::{user_layout_key, USER_FILTER_PATH};
use inputmap::Layout;
use messenger_generated::topics;
use user::central::Subscription;
use user::messenger::confd::{wire, Client};
use user::messenger::DEFAULT_BUFFER;
use user::sys;

use super::themefeed::{drain, subscribe};

/// Ticks (100 Hz) between looks at the change topic.
const POLL_TICKS: u64 = 25;
/// Ticks between attempts to read or subscribe while that failed.
const RETRY_TICKS: u64 = 300;

pub(super) struct LayoutFeed {
    /// The shell's uid, when it has a layout key of its own.
    user: Option<u32>,
    watch: Option<Subscription>,
    /// The user's key must be read again (a new user, a change, a failure).
    stale: bool,
    /// The user's own layout as last read.
    wanted: Option<Layout>,
    next_poll: u64,
    next_retry: u64,
    buffer: Vec<u8>,
}

impl LayoutFeed {
    pub(super) fn new() -> LayoutFeed {
        LayoutFeed {
            user: None,
            watch: None,
            stale: false,
            wanted: None,
            next_poll: 0,
            next_retry: 0,
            buffer: alloc::vec![0u8; DEFAULT_BUFFER],
        }
    }

    /// The layout `inputd` should use for this session (`None`: the
    /// machine default).
    pub(super) fn wanted(&self) -> Option<Layout> {
        self.wanted
    }

    /// The shell now runs as `uid`: follow that account's own layout.
    pub(super) fn follow_user(&mut self, uid: u32, confd: Option<&Client>) {
        let user = user_layout_key(uid).map(|_| uid);
        if user == self.user {
            return;
        }
        self.forget_user();
        self.user = user;
        self.stale = user.is_some();
        self.next_retry = 0;
        self.refresh(confd);
    }

    /// The session ended: back to the machine default.
    pub(super) fn forget_user(&mut self) {
        if let Some(watch) = self.watch.take() {
            let _ = watch.unsubscribe();
        }
        self.user = None;
        self.stale = false;
        self.wanted = None;
    }

    /// Follow the user's key (cheap between `POLL_TICKS`).
    pub(super) fn poll(&mut self, confd: Option<&Client>) {
        let now = sys::clock();
        if self.user.is_none() || now < self.next_poll {
            return;
        }
        self.next_poll = now + POLL_TICKS;
        if drain(&mut self.watch, &mut self.buffer) {
            self.stale = true;
        }
        self.refresh(confd);
    }

    /// Subscribe if not yet, then re-read the key if it may have changed.
    fn refresh(&mut self, confd: Option<&Client>) {
        let Some(uid) = self.user else { return };
        let now = sys::clock();
        if (self.watch.is_none() || self.stale) && now < self.next_retry {
            return;
        }
        if self.watch.is_none() {
            // Subscribed before the read, so a change in between is seen.
            self.watch = subscribe_user(uid);
            self.stale = true;
        }
        if self.stale {
            let read = confd
                .zip(user_layout_key(uid))
                .map(|(client, key)| client.get(&key));
            // A failing `confd` keeps the layout already in effect.
            if let Some(Ok(value)) = read {
                self.stale = false;
                self.set_wanted(uid, value);
            }
        }
        if self.watch.is_none() || self.stale {
            self.next_retry = now + RETRY_TICKS;
        }
    }

    fn set_wanted(&mut self, uid: u32, value: Option<confd::Value>) {
        let wanted = match value {
            Some(confd::Value::Str(name)) => Layout::from_name(&name),
            _ => None,
        };
        if wanted != self.wanted {
            self.wanted = wanted;
            let name = wanted.map_or("none", Layout::name);
            sys::write_str(&format!("XUID:LAYOUT:USER uid={uid} layout={name}\n"));
        }
    }
}

/// Subscribe to `uid`'s own input settings changes.
fn subscribe_user(uid: u32) -> Option<Subscription> {
    let uid = format!("{uid}");
    let filter = topics::build(
        wire::TOPIC_USER_CONFD_CHANGED,
        &[&uid, USER_FILTER_PATH],
        topics::Mode::Subscribe,
    )
    .ok()?;
    subscribe(filter, sys::clock())
}
