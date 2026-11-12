//! The data structures behind `messengerd`'s topics broker (`docs/messenger.md`
//! section 7.2): filter matching, QoS queues, retained values, parked pulls
//! and the per-tenant budgets. Pure `no_std` + `alloc` logic with no syscalls, so the
//! host `cargo test` drives it (unit and stress tests); `messengerd` owns the
//! wire, the policy questions to the kernel and the doorbells.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod broker;
mod filter;
mod ledger;
mod limits;
mod qos;
mod queue;
mod retained;
#[cfg(test)]
mod stress;
mod tenant;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_tenant;

pub use broker::{Broker, Pending, Pulled, Refusal, Wake};
pub use filter::{is_system_topic, valid_topic, Filter};
pub use limits::{
    MAX_BYTES, MAX_PENDING, MAX_SUBSCRIPTIONS, MAX_TOPICS, SYSTEM_RESERVE,
    SYSTEM_SUBSCRIPTION_RESERVE, SYSTEM_TENANT_BYTES, SYSTEM_TENANT_SUBSCRIPTIONS,
    SYSTEM_TOPIC_RESERVE, TENANT_BYTES, TENANT_RETAINED, TENANT_SUBSCRIPTIONS,
};
pub use messenger_generated::os_lazy_messenger_topics_v1::{Event, TopicInfo};
pub use qos::Qos;
pub use queue::{event_cost, Subscription, EVENT_OVERHEAD};
pub use tenant::{Client, Tenant};
