//! Channel counters, per-sender meters and reset.

use super::*;

/// Sum a channel's counters and live depths into `stats`.
pub(super) fn accumulate(stats: &mut Stats, channel: &Channel) {
    stats.calls += channel.calls;
    stats.replies += channel.replies;
    stats.timeouts += channel.timeouts;
    stats.cancels += channel.cancels;
    stats.drops += channel.drops;
    // Every accepted message is metered as `sent`; a synchronous call also
    // meters `calls`, so the remainder is exactly the one-way traffic.
    stats.one_way += channel
        .senders
        .iter()
        .map(|meter| meter.sent.saturating_sub(meter.calls))
        .sum::<u64>();
    for endpoint in &channel.endpoints {
        stats.queued += endpoint.inbox.len() as u64;
        stats.queued_bytes += endpoint.queued_bytes as u64;
    }
    stats.outstanding += channel
        .txns
        .iter()
        .filter(|txn| txn.state == TxnState::Pending)
        .count() as u64;
}

/// Live channel and endpoint counts for the fabric snapshot (issue #70).
pub fn counts() -> Counts {
    let channels = CHANNELS.lock();
    Counts {
        channels: channels.len() as u64,
        endpoints: channels.len() as u64 * 2,
    }
}

/// Aggregated counters and depths across every live channel.
pub fn stats() -> Stats {
    let channels = CHANNELS.lock();
    let mut stats = Stats::default();
    for channel in channels.iter() {
        accumulate(&mut stats, channel);
    }
    stats
}

/// Counters and depths for the channel `handle` names.
pub fn channel_stats(handle: u64) -> Result<Stats, Error> {
    let (channel_id, _) = endpoint_of(handle, rights::CALL)?;
    let channels = CHANNELS.lock();
    let channel = find_channel_ref(&channels, channel_id)?;
    let mut stats = Stats::default();
    accumulate(&mut stats, channel);
    Ok(stats)
}

/// Per-sender metering for the channel `handle` names.
pub fn senders(handle: u64) -> Result<Vec<SenderMeter>, Error> {
    let (channel_id, _) = endpoint_of(handle, rights::CALL)?;
    let channels = CHANNELS.lock();
    let channel = find_channel_ref(&channels, channel_id)?;
    Ok(channel.senders.clone())
}

/// Drop every channel (process teardown, reboot, test isolation), releasing
/// the buffer references held by undelivered messages.
///
/// Waiters are woken so a task parked in `await_reply` observes
/// [`Error::NoTransaction`] instead of hanging.
pub fn reset() {
    let mut channels = CHANNELS.lock();
    for channel in channels.iter() {
        for endpoint in &channel.endpoints {
            for message in &endpoint.inbox {
                release_queued(message);
                release_queued_quota(message.origin.uid, message.bytes.len());
            }
        }
    }
    channels.clear();
    drop(channels);
    MESSENGER.notify_all();
}
