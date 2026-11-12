//! `init`'s retained `system/events/service/<name>` events, followed through
//! a [`central::Feed`] (the generated typed subscription).
//!
//! The feed's doorbell is what `healthd` parks on beside its requests: an
//! idle service does not wake. The feed is made once `messengerd` serves
//! (`healthd` can start first, so the loop wakes at [`Feed::next_retry`]
//! until then), and the broker's replay of the retained rows seeds the table.

use alloc::string::String;
use alloc::vec::Vec;

use user::central::{self, Feed};
use user::messenger::{services, Endpoint};

use super::aggregate::apply_state;
use super::HealthRow;

/// The service-event feed.
pub(crate) struct ServiceFeed(Feed<services::ServiceEvent>);

impl ServiceFeed {
    pub(crate) const fn new() -> ServiceFeed {
        // Every service: the `+` wildcard of the declared pattern.
        ServiceFeed(Feed::new(|bus| {
            services::init::wire::subscribe_system_events_service(bus, "+")
        }))
    }

    /// Make the feed if it is missing.
    pub(crate) fn ensure(&mut self) {
        self.0.ensure();
    }

    /// Whether the feed is not there yet (the loop then wakes to try again).
    pub(crate) fn missing(&self) -> bool {
        self.0.missing()
    }

    /// The doorbell to park on, once the feed exists.
    pub(crate) fn bell(&mut self) -> Option<Endpoint> {
        self.0.bell()
    }

    /// Whether there is something to read without a ring: the feed is new.
    pub(crate) fn fresh(&self) -> bool {
        self.0.fresh()
    }

    /// Apply every queued `system/events/service/<name>` event when `rang`
    /// (or the feed is new).
    pub(crate) fn drain(
        &mut self,
        rang: bool,
        rows: &mut Vec<HealthRow>,
        broker: &mut central::Publisher,
        summary_state: &mut Option<(String, String)>,
    ) {
        self.0.drain(rang, |event| {
            // The broker owns the topic, so its service name is trusted over
            // any payload field.
            if let Some(name) = services::service_event_name(&event.topic) {
                apply_state(rows, name, &event.value, broker, summary_state);
            }
        });
    }
}
