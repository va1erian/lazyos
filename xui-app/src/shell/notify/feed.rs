//! Follows the notification settings in `confd` (`sys/ui/motion`,
//! `sys/ui/notify/*`, with the user's own `user/<uid>/ui/...` shadows), and
//! hands what changed to the center. Read every few seconds, like the
//! theme: a bounded `Get` per key, and a missing `confd` keeps what was
//! read last.

use lazyshell::notify::settings::{Settings, KEYS};

use super::super::ctx::Ctx;
use super::super::services;
use crate::sys;

/// How often the settings are re-read (100 Hz ticks): three seconds.
const POLL_TICKS: u64 = 300;

/// The settings as last read.
pub struct Feed {
    current: Option<Settings>,
    next_poll: u64,
}

impl Feed {
    pub fn new() -> Feed {
        Feed {
            current: None,
            next_poll: 0,
        }
    }

    /// Re-read when due and apply a change to the center.
    pub fn poll(&mut self, ctx: &Ctx) {
        let now = sys::clock_ticks();
        if now < self.next_poll {
            return;
        }
        self.next_poll = now.saturating_add(POLL_TICKS);
        let Some(next) = read(ctx.uid) else {
            return;
        };
        if self.current.as_ref() == Some(&next) {
            return;
        }
        println!(
            "SHELL:NOTIFY:SETTINGS motion={} dnd={} muted={}",
            next.motion.as_str(),
            u8::from(next.policy.dnd),
            next.policy.muted.len()
        );
        let mut center = ctx.notify.center.borrow_mut();
        center.set_motion(next.motion);
        center.set_policy(next.policy.clone());
        self.current = Some(next);
    }
}

/// The three values in effect, or `None` while `confd` does not answer.
fn read(uid: Option<u32>) -> Option<Settings> {
    let mut values = Vec::with_capacity(KEYS.len());
    for key in KEYS {
        let machine = services::confd_get(key).ok()?;
        let own = match uid.and_then(|uid| uitheme::user_key(uid, key)) {
            Some(path) => services::confd_get(&path).ok()?,
            None => None,
        };
        values.push(uitheme::overlay(machine, own));
    }
    Some(Settings::from_values(
        values[0].as_ref(),
        values[1].as_ref(),
        values[2].as_ref(),
    ))
}
