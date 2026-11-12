//! The data structures behind `messengerd`'s topics broker (`docs/messenger.md`
//! section 7.2): filter matching, QoS queues, retained values, parked pulls
//! and the byte budget. Pure `no_std` + `alloc` logic with no syscalls, so the
//! host `cargo test` drives it (unit and stress tests); `messengerd` owns the
//! wire, the policy questions to the kernel and the doorbells.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod broker;
mod filter;
mod ledger;
mod qos;
mod queue;
mod retained;
#[cfg(test)]
mod stress;
#[cfg(test)]
mod tests;

pub use broker::{
    Broker, Pending, Pulled, Refusal, Wake, MAX_BYTES, MAX_PENDING, MAX_PER_OWNER,
    MAX_SUBSCRIPTIONS, MAX_TOPICS, OWNER_BYTES, OWNER_RETAINED, SYSTEM_OWNER_BYTES, SYSTEM_RESERVE,
};
pub use filter::{is_system_topic, valid_topic, Filter};
pub use messenger_generated::os_lazy_messenger_topics_v1::{Event, TopicInfo};
pub use qos::Qos;
pub use queue::{event_cost, Subscription};
