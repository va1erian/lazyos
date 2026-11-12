//! The broker's sizes: table rows, bytes and the per-tenant shares
//! (`broker.rs` explains the model).

/// Largest number of live subscriptions the broker keeps: every service and
/// app follows its topics through it (the per-service routers are gone).
pub const MAX_SUBSCRIPTIONS: usize = 512;
/// Of [`MAX_SUBSCRIPTIONS`], the rows only platform tasks may take.
pub const SYSTEM_SUBSCRIPTION_RESERVE: usize = 128;
/// Largest number of distinct topics tracked for [`Broker::list`], and of
/// retained values kept: a retained value dropped for want of a slot is a
/// late subscriber's replay that never comes (the supervisors' per-service
/// rows alone are a hundred).
pub const MAX_TOPICS: usize = 1024;
/// Of [`MAX_TOPICS`], the rows only platform tasks may take (for both the
/// topic list and retained values).
pub const SYSTEM_TOPIC_RESERVE: usize = 256;
/// Largest number of live subscriptions one app (tenant) may hold, however
/// many threads it has.
pub const TENANT_SUBSCRIPTIONS: usize = 32;
/// The same for one platform task.
pub const SYSTEM_TENANT_SUBSCRIPTIONS: usize = 64;
/// Largest number of parked `NextEvent` transactions: one per subscription
/// at most (a new pull replaces its own stale entry), so a parked pull is
/// never refused for want of a slot.
pub const MAX_PENDING: usize = MAX_SUBSCRIPTIONS;
/// Bytes the broker holds for retained values and queued events together.
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
/// Of [`MAX_BYTES`], what only platform tasks may use.
pub const SYSTEM_RESERVE: usize = 2 * 1024 * 1024;
/// Bytes one app (tenant) may have held for it (its retained values and its
/// queues together).
pub const TENANT_BYTES: usize = 1024 * 1024;
/// The same for a platform task.
pub const SYSTEM_TENANT_BYTES: usize = 2 * 1024 * 1024;
/// Retained values one app (tenant) may keep (they cannot be removed, and they
/// outlive the task that published them).
pub const TENANT_RETAINED: usize = 32;
