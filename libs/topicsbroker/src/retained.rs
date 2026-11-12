//! Retained values: the checks that keep one publisher inside its budget and
//! the store that charges it.

use messenger_generated::os_lazy_messenger_topics_v1::Event;

use crate::broker::{Broker, Refusal, MAX_TOPICS, OWNER_RETAINED};
use crate::queue::event_cost;

impl<B: Copy> Broker<B> {
    pub(crate) fn retained_index(&self, topic: &str) -> Option<usize> {
        self.retained.iter().position(|held| held.topic == topic)
    }

    /// Whether the retained value `event` fits: a new topic needs a slot and
    /// (for an app) one of its [`OWNER_RETAINED`]; the bytes must fit the
    /// publisher's headroom, counting a value of its own it replaces by the
    /// net change.
    pub(crate) fn check_retained(&self, event: &Event, platform: bool) -> Result<(), Refusal> {
        let publisher = event.publisher;
        let mut room = self.headroom(publisher, platform);
        let count = self
            .accounts
            .get(&publisher)
            .map_or(0, |account| account.retained);
        match self.retained_index(&event.topic) {
            Some(index) => {
                let old = &self.retained[index];
                if old.publisher == publisher {
                    room = room.saturating_add(event_cost(old));
                } else if !platform && count >= OWNER_RETAINED {
                    return Err(Refusal::NoRoom);
                }
            }
            None => {
                if self.retained.len() >= MAX_TOPICS || (!platform && count >= OWNER_RETAINED) {
                    return Err(Refusal::NoRoom);
                }
            }
        }
        if event_cost(event) > room {
            return Err(Refusal::NoRoom);
        }
        Ok(())
    }

    /// Remember (or replace) the retained value for a topic; checked by
    /// [`Broker::check_retained`] first.
    pub(crate) fn store_retained(&mut self, event: &Event) {
        let cost = event_cost(event);
        match self.retained_index(&event.topic) {
            Some(index) => {
                let old_owner = self.retained[index].publisher;
                let old = event_cost(&self.retained[index]);
                self.retained[index] = event.clone();
                if old_owner == event.publisher {
                    self.rebook(old_owner, old, cost);
                } else {
                    self.rebook(old_owner, old, 0);
                    self.rebook_retained(old_owner, false);
                    self.rebook(event.publisher, 0, cost);
                    self.rebook_retained(event.publisher, true);
                }
            }
            None => {
                self.retained.push(event.clone());
                self.rebook(event.publisher, 0, cost);
                self.rebook_retained(event.publisher, true);
            }
        }
    }
}
