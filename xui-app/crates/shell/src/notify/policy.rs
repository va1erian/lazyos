//! Who may notify, who they are shown as, and how much they may say
//! (docs/notify-plan.md, "Policy").
//!
//! Identity comes from the kernel, as for the tray: the session and uid it
//! stamped on the request and the sender's task, which `init`'s table maps
//! to the app it launched. Nothing in the request names the sender, so an
//! app cannot post as another app or as the system.
//!
//! A caller that is no launched app but is in the shell's login session, runs
//! as the shell's user and carries no package label (a script run from the
//! Terminal) is shown as [`SCRIPT_OWNER`]: the shell chooses that name, the
//! caller never does. It is refused outside a login session (session 0 is
//! where the system services run).

use crate::tray::policy::{app_of, check, Caller, Deny, Shell};

/// The owner key of the shell's own toasts (the app-stopped notice). Not an
/// app id a package may have: it has no dot-separated reverse-DNS form.
pub const SYSTEM_OWNER: &str = "<system>";
/// The owner key of a script run from the Terminal.
pub const SCRIPT_OWNER: &str = "<terminal>";
/// The name shown for [`SCRIPT_OWNER`].
pub const SCRIPT_NAME: &str = "Terminal";
/// The name shown for [`SYSTEM_OWNER`].
pub const SYSTEM_NAME: &str = "LazyOS";

/// Longest owner key kept.
pub const KEY_MAX: usize = 128;

/// Notifications one sender may post in a burst before the rate limit
/// applies, and the ticks (100 Hz) after which one more is allowed.
pub const RATE_BURST: u32 = 6;
pub const RATE_REFILL_TICKS: u64 = 50;
/// Most senders whose bucket is remembered.
pub const BUCKETS_MAX: usize = 64;

/// Who a request is from, once it passed the checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner<'a> {
    /// An app `init` launched, by its app id.
    App(&'a str),
    /// An unlabelled script in the shell's session.
    Script,
}

impl Owner<'_> {
    /// The key the center keeps the sender's toasts under.
    pub fn key(self) -> String {
        match self {
            Owner::App(app) => app.to_owned(),
            Owner::Script => SCRIPT_OWNER.to_owned(),
        }
    }
}

/// The sender of a request: session and uid as the kernel stamped them, the
/// package label (0 for an unlabelled program) and the task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sender {
    pub caller: Caller,
    pub label: u32,
    pub pid: u64,
}

/// Decide who `sender` is, from `init`'s `(name, pid)` rows.
pub fn identify<'a>(
    sender: Sender,
    shell: Shell,
    rows: impl IntoIterator<Item = (&'a str, u64)>,
    is_app: impl Fn(&str) -> bool,
) -> Result<Owner<'a>, Deny> {
    check(sender.caller, shell)?;
    match app_of(rows, sender.pid, is_app) {
        Ok(app) => Ok(Owner::App(app)),
        Err(Deny::NotAnApp) if sender.label == 0 && shell.session != Some(0) => Ok(Owner::Script),
        Err(why) => Err(why),
    }
}

/// The user's do-not-disturb and per-app mute (`sys/ui/notify/*`), which
/// the presenter honours: a sender cannot tell and cannot override.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    /// Only Error toasts are shown.
    pub dnd: bool,
    /// Owner keys whose toasts are never shown.
    pub muted: Vec<String>,
}

impl Policy {
    /// Whether a toast of `level` from `owner` is held back. The shell's own
    /// toasts are never held back: they are how the user hears of a crash.
    pub fn suppresses(&self, owner: &str, level: super::note::Level) -> bool {
        if owner == SYSTEM_OWNER {
            return false;
        }
        self.muted.iter().any(|muted| muted == owner)
            || (self.dnd && level != super::note::Level::Error)
    }

    /// Parse the `sys/ui/notify/mute` value: owner keys separated by
    /// whitespace or commas.
    pub fn parse_muted(value: &str) -> Vec<String> {
        value
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|key| !key.is_empty() && key.len() <= KEY_MAX)
            .map(str::to_owned)
            .collect()
    }
}

/// One sender's token bucket: [`RATE_BURST`] at most, one more every
/// [`RATE_REFILL_TICKS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bucket {
    tokens: u32,
    /// The tick `tokens` was last brought up to date.
    at: u64,
}

/// The senders' buckets.
#[derive(Debug, Default)]
pub struct RateLimit {
    buckets: Vec<(String, Bucket)>,
}

impl RateLimit {
    /// Take one token for `owner` at `now`; `false` when it is out of them.
    pub fn take(&mut self, owner: &str, now: u64) -> bool {
        let index = match self.buckets.iter().position(|(key, _)| key == owner) {
            Some(index) => index,
            None => {
                if self.buckets.len() >= BUCKETS_MAX {
                    self.make_room(now);
                }
                self.buckets.push((
                    owner.to_owned(),
                    Bucket {
                        tokens: RATE_BURST,
                        at: now,
                    },
                ));
                self.buckets.len() - 1
            }
        };
        let bucket = &mut self.buckets[index].1;
        let earned = now.saturating_sub(bucket.at) / RATE_REFILL_TICKS;
        if earned > 0 {
            bucket.tokens = (u64::from(bucket.tokens) + earned).min(u64::from(RATE_BURST)) as u32;
            bucket.at = if bucket.tokens == RATE_BURST {
                now
            } else {
                bucket.at + earned * RATE_REFILL_TICKS
            };
        }
        if bucket.tokens == 0 {
            return false;
        }
        bucket.tokens -= 1;
        true
    }

    /// Forget a bucket: a full one first (it is the same as a fresh one),
    /// else the one least recently used.
    fn make_room(&mut self, now: u64) {
        let full = self.buckets.iter().position(|(_, bucket)| {
            u64::from(bucket.tokens) + now.saturating_sub(bucket.at) / RATE_REFILL_TICKS
                >= u64::from(RATE_BURST)
        });
        let index = full.unwrap_or_else(|| {
            self.buckets
                .iter()
                .enumerate()
                .min_by_key(|(_, (_, bucket))| bucket.at)
                .map_or(0, |(index, _)| index)
        });
        self.buckets.remove(index);
    }

    /// Senders with a remembered bucket.
    pub fn len(&self) -> usize {
        self.buckets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }
}
