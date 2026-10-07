//! The password-guessing brake (docs/accounts-plan.md U1): `Authenticate`,
//! `SetPassword`'s old-password check and `elevd`'s admin prompt are slowed
//! per account name and per caller.
//!
//! Each key (an account name, or a calling uid) may fail [`FREE_FAILURES`]
//! times in a row for free; every further failure locks the key for a delay
//! that doubles from [`BASE_DELAY`] up to [`MAX_DELAY`]. While a key is
//! locked, an attempt is refused at once (`EAGAIN`), without asking `keyd`,
//! so a flood costs the attacker time and the machine nothing. A key idle
//! for [`FORGET_AFTER`] starts again from zero.
//!
//! Three rules keep the brake from becoming a way to lock others out or to
//! reset one's own lock (review of #659, H5):
//!
//! * **who counts where** ([`Attempt`]): an attempt is *checked* against the
//!   name and, for an ordinary caller, its uid; a failure is *counted* under
//!   the name only for a mediator (`logind`, `elevd`, which ask for somebody
//!   at a keyboard and slow their own askers), and under the caller's uid
//!   only otherwise. A session flooding `Authenticate("admin", ...)` locks
//!   itself, never `admin` for the login screen or the trusted prompt;
//! * **a success clears names only** ([`Limiter::succeeded`]): the name that
//!   authenticated starts again from zero, a caller's lock never does, so
//!   knowing one password does not buy more guesses at another;
//! * **a locked slot is never evicted** ([`MAX_SLOTS`]): when the table is
//!   full, a forgotten slot or the least recently used one still within its
//!   free failures makes room; when every slot holds a penalty, an attempt
//!   under a key the table cannot hold is refused until one is forgotten
//!   ([`Limiter::check`]), so filling the table cannot wipe the lock that
//!   slows a guesser.
//!
//! Times are kernel ticks (100 Hz).

use alloc::string::String;
use alloc::vec::Vec;

/// Failures in a row a key may have before it is slowed down.
pub const FREE_FAILURES: u32 = 3;
/// The first lock, after the free failures: 1 s.
pub const BASE_DELAY: u64 = 100;
/// The longest lock: 60 s.
pub const MAX_DELAY: u64 = 6000;
/// A key idle this long is forgotten: 15 min.
pub const FORGET_AFTER: u64 = 90_000;
/// Most keys tracked at once.
pub const MAX_SLOTS: usize = 128;

/// What an attempt is counted under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// The account the password is tried for.
    Name(String),
    /// The uid of the task trying it.
    Caller(u32),
}

/// The keys of one password attempt (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attempt {
    /// Every key whose lock refuses the attempt.
    pub checked: Vec<Key>,
    /// The keys a failure counts under.
    pub counted: Vec<Key>,
    /// The key a success clears: the name, never the caller.
    pub cleared: Option<Key>,
}

impl Attempt {
    /// The attempt on `name` by the task with `uid`; `mediator` when it
    /// checks passwords for a person at the keyboard (`logind`, `elevd`).
    /// An invalid name names no account: it is neither checked nor counted,
    /// or a guesser could fill the table with junk names.
    pub fn new(name: &str, uid: u32, mediator: bool) -> Attempt {
        let name = crate::valid_name(name).then(|| Key::Name(String::from(name)));
        let caller = (!mediator).then_some(Key::Caller(uid));
        Attempt {
            checked: name.iter().chain(caller.iter()).cloned().collect(),
            counted: if mediator {
                name.iter().cloned().collect()
            } else {
                caller.iter().cloned().collect()
            },
            cleared: name,
        }
    }
}

#[derive(Clone, Debug)]
struct Slot {
    key: Key,
    failures: u32,
    /// Locked until this tick.
    until: u64,
    /// The last attempt.
    last: u64,
}

impl Slot {
    fn stale(&self, now: u64) -> bool {
        now.saturating_sub(self.last) >= FORGET_AFTER
    }

    /// Locked now, or past its free failures (between two locks): either
    /// way the slot holds a penalty a guesser would love to see dropped.
    fn penalized(&self, now: u64) -> bool {
        !self.stale(now) && (self.until > now || self.failures > FREE_FAILURES)
    }
}

/// The brake's state.
#[derive(Clone, Debug, Default)]
pub struct Limiter {
    slots: Vec<Slot>,
}

/// The lock that follows the `failures`-th failure in a row.
pub fn delay(failures: u32) -> u64 {
    if failures <= FREE_FAILURES {
        return 0;
    }
    let doublings = (failures - FREE_FAILURES - 1).min(16);
    (BASE_DELAY << doublings).min(MAX_DELAY)
}

impl Limiter {
    pub fn new() -> Limiter {
        Limiter::default()
    }

    /// `Ok` when an attempt under every key in `keys` may go ahead now;
    /// otherwise the ticks until it may. A key the full table could not
    /// record waits for the first locked slot to free.
    pub fn check(&self, keys: &[Key], now: u64) -> Result<(), u64> {
        let mut wait = keys
            .iter()
            .filter_map(|key| self.slot(key, now))
            .map(|slot| slot.until.saturating_sub(now))
            .max()
            .unwrap_or(0);
        let untracked = keys.iter().any(|key| self.position(key).is_none());
        if untracked && self.slots.len() >= MAX_SLOTS && self.evictable(now).is_none() {
            let free = self
                .slots
                .iter()
                .map(|slot| slot.until.saturating_sub(now))
                .min()
                .unwrap_or(0);
            wait = wait.max(free.max(1));
        }
        if wait == 0 {
            Ok(())
        } else {
            Err(wait)
        }
    }

    /// An attempt under `keys` failed. A key the table has no room for
    /// (every slot locked) is not recorded; [`Limiter::check`] refuses
    /// attempts under it until there is.
    pub fn failed(&mut self, keys: &[Key], now: u64) {
        for key in keys {
            let Some(index) = self.index_or_insert(key, now) else {
                continue;
            };
            let slot = &mut self.slots[index];
            if slot.stale(now) {
                slot.failures = 0;
            }
            slot.failures = slot.failures.saturating_add(1);
            slot.last = now;
            slot.until = now.saturating_add(delay(slot.failures));
        }
    }

    /// An attempt under `keys` succeeded: the account names among them start
    /// again from zero. A caller's key is never cleared by a success (see
    /// the module docs).
    pub fn succeeded(&mut self, keys: &[Key]) {
        self.slots
            .retain(|slot| !(matches!(slot.key, Key::Name(_)) && keys.contains(&slot.key)));
    }

    /// Failures in a row recorded for `key` (0 when unknown or forgotten).
    pub fn failures(&self, key: &Key, now: u64) -> u32 {
        self.slot(key, now).map_or(0, |slot| slot.failures)
    }

    /// How many keys are tracked.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether no key is tracked.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// The live slot of `key`, if it is not forgotten yet.
    fn slot(&self, key: &Key, now: u64) -> Option<&Slot> {
        self.slots
            .iter()
            .find(|slot| &slot.key == key && !slot.stale(now))
    }

    fn position(&self, key: &Key) -> Option<usize> {
        self.slots.iter().position(|slot| &slot.key == key)
    }

    fn index_or_insert(&mut self, key: &Key, now: u64) -> Option<usize> {
        if let Some(index) = self.position(key) {
            return Some(index);
        }
        if self.slots.len() >= MAX_SLOTS {
            let index = self.evictable(now)?;
            self.slots.swap_remove(index);
        }
        self.slots.push(Slot {
            key: key.clone(),
            failures: 0,
            until: 0,
            last: now,
        });
        Some(self.slots.len() - 1)
    }

    /// The slot that makes room: a forgotten one, else the least recently
    /// used one without a penalty. Never a locked slot, nor one between two
    /// locks.
    fn evictable(&self, now: u64) -> Option<usize> {
        let stale = self.slots.iter().position(|slot| slot.stale(now));
        stale.or_else(|| {
            self.slots
                .iter()
                .enumerate()
                .filter(|(_, slot)| !slot.penalized(now))
                .min_by_key(|(_, slot)| slot.last)
                .map(|(index, _)| index)
        })
    }
}
