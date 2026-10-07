//! The password-guessing brake (docs/accounts-plan.md U1): `Authenticate`,
//! `SetPassword`'s old-password check and `elevd`'s admin prompt are slowed
//! per account name and per caller.
//!
//! Each key (an account name, or a calling uid) may fail [`FREE_FAILURES`]
//! times in a row for free; every further failure locks the key for a delay
//! that doubles from [`BASE_DELAY`] up to [`MAX_DELAY`]. While a key is
//! locked, an attempt is refused at once (`EAGAIN`), without asking `keyd`,
//! so a flood costs the attacker time and the machine nothing. A success
//! clears the keys it was checked under; a key idle for [`FORGET_AFTER`]
//! starts again from zero. The table is bounded ([`MAX_SLOTS`]): when full,
//! the least recently used unlocked name slot goes first, so flooding many
//! names cannot evict the slot that slows down one caller.
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

#[derive(Clone, Debug)]
struct Slot {
    key: Key,
    failures: u32,
    /// Locked until this tick.
    until: u64,
    /// The last attempt.
    last: u64,
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
    /// otherwise the ticks until the last of them unlocks.
    pub fn check(&self, keys: &[Key], now: u64) -> Result<(), u64> {
        let wait = keys
            .iter()
            .filter_map(|key| self.slot(key, now))
            .map(|slot| slot.until.saturating_sub(now))
            .max()
            .unwrap_or(0);
        if wait == 0 {
            Ok(())
        } else {
            Err(wait)
        }
    }

    /// An attempt under `keys` failed.
    pub fn failed(&mut self, keys: &[Key], now: u64) {
        for key in keys {
            let index = self.index_or_insert(key, now);
            let slot = &mut self.slots[index];
            if now.saturating_sub(slot.last) >= FORGET_AFTER {
                slot.failures = 0;
            }
            slot.failures = slot.failures.saturating_add(1);
            slot.last = now;
            slot.until = now.saturating_add(delay(slot.failures));
        }
    }

    /// An attempt under `keys` succeeded: they start again from zero.
    pub fn succeeded(&mut self, keys: &[Key]) {
        self.slots.retain(|slot| !keys.contains(&slot.key));
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
            .find(|slot| &slot.key == key && now.saturating_sub(slot.last) < FORGET_AFTER)
    }

    fn index_or_insert(&mut self, key: &Key, now: u64) -> usize {
        if let Some(index) = self.slots.iter().position(|slot| &slot.key == key) {
            return index;
        }
        if self.slots.len() >= MAX_SLOTS {
            self.evict(now);
        }
        self.slots.push(Slot {
            key: key.clone(),
            failures: 0,
            until: 0,
            last: now,
        });
        self.slots.len() - 1
    }

    /// Make room: a forgotten slot, else the least recently used unlocked
    /// name, else the least recently used slot of all.
    fn evict(&mut self, now: u64) {
        let stale = self
            .slots
            .iter()
            .position(|slot| now.saturating_sub(slot.last) >= FORGET_AFTER);
        let unlocked_name = || {
            self.slots
                .iter()
                .enumerate()
                .filter(|(_, slot)| matches!(slot.key, Key::Name(_)) && slot.until <= now)
                .min_by_key(|(_, slot)| slot.last)
                .map(|(index, _)| index)
        };
        let oldest = || {
            self.slots
                .iter()
                .enumerate()
                .min_by_key(|(_, slot)| slot.last)
                .map(|(index, _)| index)
        };
        if let Some(index) = stale.or_else(unlocked_name).or_else(oldest) {
            self.slots.swap_remove(index);
        }
    }
}
