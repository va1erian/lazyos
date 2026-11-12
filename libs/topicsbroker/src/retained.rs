//! Retained values: the checks that keep one tenant inside its budget and the
//! store that charges it.

use messenger_generated::os_lazy_messenger_topics_v1::Event;

use crate::broker::{Broker, Refusal};
use crate::limits::{MAX_TOPICS, SYSTEM_TOPIC_RESERVE, TENANT_RETAINED};
use crate::queue::event_cost;
use crate::tenant::Tenant;

/// A retained value and the tenant it is charged to. The charge stays with
/// the tenant after the publishing task exits: a retained value outlives its
/// publisher, and so does its cost.
pub(crate) struct Retained {
    pub(crate) event: Event,
    pub(crate) tenant: Tenant,
}

impl<B: Copy> Broker<B> {
    pub(crate) fn retained_index(&self, topic: &str) -> Option<usize> {
        self.retained
            .iter()
            .position(|held| held.event.topic == topic)
    }

    /// Whether the retained value `event` fits: a new topic needs a row (an
    /// app may not take the last [`SYSTEM_TOPIC_RESERVE`]) and, for an app,
    /// one of its tenant's [`TENANT_RETAINED`]; the bytes must fit the
    /// tenant's headroom, counting a value of its own it replaces by the net
    /// change.
    pub(crate) fn check_retained(&self, event: &Event, tenant: Tenant) -> Result<(), Refusal> {
        let platform = tenant.is_platform();
        let mut room = self.headroom(tenant);
        let at_cap = !platform && self.retained_by(&tenant) >= TENANT_RETAINED;
        match self.retained_index(&event.topic) {
            Some(index) => {
                let old = &self.retained[index];
                if old.tenant == tenant {
                    room = room.saturating_add(event_cost(&old.event));
                } else if at_cap {
                    return Err(Refusal::NoRoom);
                }
            }
            None => {
                let rows = if platform {
                    MAX_TOPICS
                } else {
                    MAX_TOPICS - SYSTEM_TOPIC_RESERVE
                };
                if self.retained.len() >= rows || at_cap {
                    return Err(Refusal::NoRoom);
                }
            }
        }
        if event_cost(event) > room {
            return Err(Refusal::NoRoom);
        }
        Ok(())
    }

    /// Remember (or replace) the retained value for a topic, charged to
    /// `tenant`; checked by [`Broker::check_retained`] first.
    pub(crate) fn store_retained(&mut self, event: &Event, tenant: Tenant) {
        let cost = event_cost(event);
        let held = Retained {
            event: event.clone(),
            tenant,
        };
        match self.retained_index(&event.topic) {
            Some(index) => {
                let old_tenant = self.retained[index].tenant;
                let old = event_cost(&self.retained[index].event);
                self.retained[index] = held;
                if old_tenant == tenant {
                    self.rebook(tenant, old, cost);
                } else {
                    self.rebook(old_tenant, old, 0);
                    self.rebook_retained(old_tenant, false);
                    self.rebook(tenant, 0, cost);
                    self.rebook_retained(tenant, true);
                }
            }
            None => {
                self.retained.push(held);
                self.rebook(tenant, 0, cost);
                self.rebook_retained(tenant, true);
            }
        }
    }
}
