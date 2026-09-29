//! Test-only hooks for the kernel suite (issue #62 pattern), compiled only
//! with `LAZYOS_TESTS=1`.
//!
//! The channel tests need to park a task exactly where `recv` parks it,
//! without entering the scheduler (the suite runs with interrupts disabled),
//! and to observe the per-endpoint waiter lists behind targeted wakeups
//! (issue #338).

use super::*;

/// Park `slot` on the messenger wait queue without switching context.
pub fn park(slot: usize, deadline: Option<u64>) {
    MESSENGER.park(slot, deadline);
}

/// Park `slot` as if it had called `recv` on `handle` (a handle in the
/// *current* task's table) and found the inbox empty: register it on the
/// endpoint and block it on the messenger queue, without switching context.
pub fn park_receiver(slot: usize, handle: u64, deadline: Option<u64>) -> Result<(), Error> {
    let (channel_id, side) = endpoint_of(handle, rights::CALL)?;
    {
        let mut channels = CHANNELS.lock();
        let channel = find_channel(&mut channels, channel_id)?;
        add_waiter(&mut channel.endpoints[side], slot);
    }
    MESSENGER.park(slot, deadline);
    Ok(())
}

/// The task slots registered as parked on `handle`'s endpoint.
pub fn endpoint_waiters(handle: u64) -> Result<Vec<usize>, Error> {
    let (channel_id, side) = endpoint_of(handle, rights::CALL)?;
    let channels = CHANNELS.lock();
    let channel = find_channel_ref(&channels, channel_id)?;
    Ok(channel.endpoints[side].waiters.clone())
}

/// Registrations across every live endpoint (a leak never returns to 0).
pub fn total_waiters() -> usize {
    CHANNELS
        .lock()
        .iter()
        .flat_map(|channel| channel.endpoints.iter())
        .map(|endpoint| endpoint.waiters.len())
        .sum()
}

/// Entries on the messenger wait queue, duplicates included.
pub fn queued_waiters() -> usize {
    MESSENGER.len()
}

/// Whether `slot` is enqueued on the messenger wait queue.
pub fn is_queued(slot: usize) -> bool {
    MESSENGER.contains(slot)
}
