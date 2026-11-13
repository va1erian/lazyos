//! The per-tenant byte ledger of the [`Broker`]: who is charged for what, how
//! much room a tenant has left, and the change wrapper that keeps the ledger
//! in step with every queue mutation.

#[cfg(test)]
use alloc::collections::BTreeMap;
#[cfg(test)]
use alloc::string::String;
use messenger_generated::os_lazy_messenger_topics_v1::Event;

use crate::broker::Broker;
use crate::limits::{MAX_BYTES, SYSTEM_RESERVE, SYSTEM_TENANT_BYTES, TENANT_BYTES};
#[cfg(test)]
use crate::queue::event_cost;
use crate::queue::{take_bytes, Subscription};
use crate::tenant::Tenant;

/// What one tenant has held for it.
#[derive(Default)]
pub(crate) struct Account {
    /// Bytes of its retained values and queued events.
    pub(crate) bytes: usize,
    /// Retained values its tasks published.
    pub(crate) retained: usize,
}

impl<B: Copy> Broker<B> {
    /// How many more bytes `tenant` may have held: what is left of its own
    /// allowance or of the pool it draws on, whichever is less. An app's pool
    /// stops short of the system reserve.
    pub(crate) fn headroom(&self, tenant: Tenant) -> usize {
        let (allowance, pool) = if tenant.is_platform() {
            (SYSTEM_TENANT_BYTES, MAX_BYTES)
        } else {
            (TENANT_BYTES, MAX_BYTES - SYSTEM_RESERVE)
        };
        allowance
            .saturating_sub(self.bytes_of(&tenant))
            .min(pool.saturating_sub(self.used))
    }

    /// Move `tenant`'s held bytes from `before` to `after`.
    pub(crate) fn rebook(&mut self, tenant: Tenant, before: usize, after: usize) {
        if before == after {
            return;
        }
        let account = self.accounts.entry(tenant).or_default();
        account.bytes = take_bytes(account.bytes, before) + after;
        self.used = take_bytes(self.used, before) + after;
        self.prune(tenant);
    }

    /// Count `tenant`'s retained values up or down.
    pub(crate) fn rebook_retained(&mut self, tenant: Tenant, gained: bool) {
        let account = self.accounts.entry(tenant).or_default();
        account.retained = if gained {
            account.retained + 1
        } else {
            take_bytes(account.retained, 1)
        };
        self.prune(tenant);
    }

    /// Forget an account that holds nothing.
    pub(crate) fn prune(&mut self, tenant: Tenant) {
        if self
            .accounts
            .get(&tenant)
            .is_some_and(|account| account.bytes == 0 && account.retained == 0)
        {
            self.accounts.remove(&tenant);
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
        let (tenant, before) = (sub.tenant, sub.bytes);
        let result = change(sub);
        let after = sub.bytes;
        self.rebook(tenant, before, after);
        result
    }

    /// Queue `event` on subscription `index` within its tenant's headroom.
    pub(crate) fn deliver(&mut self, index: usize, event: Event) {
        let sub = &self.subscriptions[index];
        let room = self.headroom(sub.tenant);
        self.change(index, |sub| sub.push(event, room));
    }

    /// Recompute the ledger from the tables and compare it with the running
    /// one: the stress tests call this after every step.
    #[cfg(test)]
    pub(crate) fn audit(&self) -> Result<(), String> {
        let mut expected: BTreeMap<Tenant, (usize, usize)> = BTreeMap::new();
        for sub in &self.subscriptions {
            expected.entry(sub.tenant).or_default().0 += sub.bytes;
        }
        for held in &self.retained {
            let entry = expected.entry(held.tenant).or_default();
            entry.0 += event_cost(&held.event);
            entry.1 += 1;
        }
        expected.retain(|_, (bytes, count)| *bytes != 0 || *count != 0);
        let actual: BTreeMap<Tenant, (usize, usize)> = self
            .accounts
            .iter()
            .map(|(tenant, a)| (*tenant, (a.bytes, a.retained)))
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
