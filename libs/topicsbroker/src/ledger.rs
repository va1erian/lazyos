//! The per-owner byte ledger of the [`Broker`]: who is charged for what, how
//! much room an owner has left, and the change wrapper that keeps the ledger
//! in step with every queue mutation.

#[cfg(test)]
use alloc::collections::BTreeMap;
#[cfg(test)]
use alloc::string::String;
use messenger_generated::os_lazy_messenger_topics_v1::Event;

use crate::broker::{Broker, MAX_BYTES, OWNER_BYTES, SYSTEM_OWNER_BYTES, SYSTEM_RESERVE};
#[cfg(test)]
use crate::queue::event_cost;
use crate::queue::{take_bytes, Subscription};

/// What one task has held for it.
#[derive(Default)]
pub(crate) struct Account {
    /// Bytes of its retained values and queued events.
    pub(crate) bytes: usize,
    /// Retained values it published.
    pub(crate) retained: usize,
}

impl<B: Copy> Broker<B> {
    /// How many more bytes `owner` may have held: what is left of its own
    /// allowance or of the pool it draws on, whichever is less. A non-platform
    /// task's pool stops short of the system reserve.
    pub(crate) fn headroom(&self, owner: u64, platform: bool) -> usize {
        let (allowance, pool) = if platform {
            (SYSTEM_OWNER_BYTES, MAX_BYTES)
        } else {
            (OWNER_BYTES, MAX_BYTES - SYSTEM_RESERVE)
        };
        allowance
            .saturating_sub(self.bytes_of(owner))
            .min(pool.saturating_sub(self.used))
    }

    /// Move `owner`'s held bytes from `before` to `after`.
    pub(crate) fn rebook(&mut self, owner: u64, before: usize, after: usize) {
        if before == after {
            return;
        }
        let account = self.accounts.entry(owner).or_default();
        account.bytes = take_bytes(account.bytes, before) + after;
        self.used = take_bytes(self.used, before) + after;
        self.prune(owner);
    }

    /// Count `owner`'s retained values up or down.
    pub(crate) fn rebook_retained(&mut self, owner: u64, gained: bool) {
        let account = self.accounts.entry(owner).or_default();
        account.retained = if gained {
            account.retained + 1
        } else {
            take_bytes(account.retained, 1)
        };
        self.prune(owner);
    }

    /// Forget an account that holds nothing.
    pub(crate) fn prune(&mut self, owner: u64) {
        if self
            .accounts
            .get(&owner)
            .is_some_and(|account| account.bytes == 0 && account.retained == 0)
        {
            self.accounts.remove(&owner);
        }
    }

    /// Run `change` on subscription `index` and rebook whatever bytes it
    /// added or freed, so the ledger cannot drift from the queues.
    pub(crate) fn change<R>(
        &mut self,
        index: usize,
        change: impl FnOnce(&mut Subscription<B>) -> R,
    ) -> R {
        let sub = &mut self.subscriptions[index];
        let (owner, before) = (sub.owner, sub.bytes);
        let result = change(sub);
        let after = sub.bytes;
        self.rebook(owner, before, after);
        result
    }

    /// Queue `event` on subscription `index` within its owner's headroom.
    pub(crate) fn deliver(&mut self, index: usize, event: Event) {
        let sub = &self.subscriptions[index];
        let room = self.headroom(sub.owner, sub.platform);
        self.change(index, |sub| sub.push(event, room));
    }

    /// Recompute the ledger from the tables and compare it with the running
    /// one: the stress tests call this after every step.
    #[cfg(test)]
    pub(crate) fn audit(&self) -> Result<(), String> {
        let mut expected: BTreeMap<u64, (usize, usize)> = BTreeMap::new();
        for sub in &self.subscriptions {
            expected.entry(sub.owner).or_default().0 += sub.bytes;
        }
        for event in &self.retained {
            let entry = expected.entry(event.publisher).or_default();
            entry.0 += event_cost(event);
            entry.1 += 1;
        }
        expected.retain(|_, (bytes, count)| *bytes != 0 || *count != 0);
        let actual: BTreeMap<u64, (usize, usize)> = self
            .accounts
            .iter()
            .map(|(owner, a)| (*owner, (a.bytes, a.retained)))
            .collect();
        if expected != actual {
            return Err(alloc::format!("ledger {actual:?} != tables {expected:?}"));
        }
        let total: usize = expected.values().map(|(bytes, _)| bytes).sum();
        if total != self.used {
            return Err(alloc::format!("used {} != {total}", self.used));
        }
        for sub in &self.subscriptions {
            let held: usize = sub.held_events().map(event_cost).sum();
            if held != sub.bytes {
                return Err(alloc::format!(
                    "sub {} holds {held}, says {}",
                    sub.id,
                    sub.bytes
                ));
            }
        }
        Ok(())
    }
}
