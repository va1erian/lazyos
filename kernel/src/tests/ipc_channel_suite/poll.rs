//! Poll calls (`channels::POLL_DEADLINE`): answered within the callee's
//! service turn, ended when the callee returns to `recv`, bounded otherwise.

use super::*;

const POLL: Option<u64> = Some(channels::POLL_DEADLINE);

/// A poll is not dead on arrival: the callee receives it and its reply is
/// accepted and returned. (A literally expired call would be `TimedOut` the
/// moment the caller awaited, and this reply refused.)
pub fn poll_answered_in_service_turn() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let request = parcel(7, flags::SYNC, "anything queued?")?;
    let txn = channels::begin_call(client, 7, &request, POLL).map_err(reason)?;
    let message = channels::try_recv(server).map_err(reason)?;
    check!(
        message.as_ref().and_then(|m| m.txn) == Some(txn),
        "the callee did not receive the poll"
    );
    let answer = parcel(8, 0, "yes")?;
    channels::reply(txn, &answer).map_err(reason)?;
    let got = channels::await_reply(txn).map_err(reason)?;
    check!(got == answer, "the poll did not return the callee's reply");
    check!(
        channels::stats().timeouts == 0,
        "an answered poll was counted as timed out"
    );
    fresh()
}

/// A poll the callee receives but defers (a parked pull) ends with `TimedOut`
/// as soon as the callee returns to `recv`, and a late reply is refused.
pub fn poll_unanswered_ends_on_next_recv() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let request = parcel(7, flags::SYNC, "park me")?;
    let txn = channels::begin_call(client, 7, &request, POLL).map_err(reason)?;
    check!(
        channels::try_recv(server).map_err(reason)?.is_some(),
        "the callee did not receive the poll"
    );
    // Still serving: nothing has ended the poll yet.
    check!(
        channels::stats().outstanding == 1,
        "the poll ended while the callee was still serving it"
    );
    check!(
        channels::try_recv(server).map_err(reason)?.is_none(),
        "unexpected second message"
    );
    let late = parcel(8, 0, "late")?;
    check!(
        channels::reply(txn, &late) == Err(ChannelError::NoTransaction),
        "a reply to an abandoned poll was accepted"
    );
    check!(
        channels::await_reply(txn) == Err(ChannelError::TimedOut),
        "an abandoned poll did not report TimedOut"
    );
    let stats = channels::stats();
    check!(
        stats.timeouts == 1 && stats.outstanding == 0,
        "counters after an abandoned poll: {stats:?}"
    );
    fresh()
}

/// A poll the callee never receives is bounded by the grace period, so a
/// stalled service cannot stall its pollers.
pub fn poll_grace_bounds_unreceived() -> Result<(), String> {
    fresh()?;
    let (client, _server) = channels::create().map_err(reason)?;
    let request = parcel(7, flags::SYNC, "nobody listening")?;
    let start = task::ticks();
    let txn = channels::begin_call(client, 7, &request, POLL).map_err(reason)?;
    channels::expire_deadlines(start);
    check!(
        channels::stats().outstanding == 1,
        "the poll expired before its grace period"
    );
    channels::expire_deadlines(start + channels::POLL_GRACE_TICKS + 1);
    check!(
        channels::await_reply(txn) == Err(ChannelError::TimedOut),
        "an unreceived poll outlived its grace period"
    );
    fresh()
}

/// Many polls, half answered and half deferred: every one ends with the
/// right outcome and nothing stays outstanding.
pub fn poll_soak() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let request = parcel(7, flags::SYNC, "soak")?;
    let answer = parcel(8, 0, "ok")?;
    let rounds = 20_000u64;
    for round in 0..rounds {
        let txn = channels::begin_call(client, 7, &request, POLL).map_err(reason)?;
        check!(
            channels::try_recv(server).map_err(reason)?.is_some(),
            "round {round}: the callee missed the poll"
        );
        if round % 2 == 0 {
            channels::reply(txn, &answer).map_err(reason)?;
            check!(
                channels::await_reply(txn) == Ok(answer.clone()),
                "round {round}: the answered poll lost its reply"
            );
        } else {
            // Defer, then come back to `recv`: the poll ends.
            check!(
                channels::try_recv(server).map_err(reason)?.is_none(),
                "round {round}: stray message"
            );
            check!(
                channels::await_reply(txn) == Err(ChannelError::TimedOut),
                "round {round}: the deferred poll did not time out"
            );
        }
    }
    let stats = channels::stats();
    check!(
        stats.timeouts == rounds / 2 && stats.outstanding == 0 && stats.queued == 0,
        "counters after the poll soak: {stats:?}"
    );
    fresh()
}

/// Receipt ends the grace deadline: a poll the callee has received survives
/// past its original grace period, so a slow service turn can still reply,
/// while a callee that wedges mid-request is still bounded.
pub fn poll_received_outlives_grace() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let request = parcel(7, flags::SYNC, "slow service")?;
    let start = task::ticks();
    let txn = channels::begin_call(client, 7, &request, POLL).map_err(reason)?;
    check!(
        channels::try_recv(server).map_err(reason)?.is_some(),
        "the callee did not receive the poll"
    );
    // The original grace deadline passes while the callee is still serving.
    channels::expire_deadlines(start + channels::POLL_GRACE_TICKS + 1);
    let answer = parcel(8, 0, "slow but here")?;
    channels::reply(txn, &answer).map_err(reason)?;
    check!(
        channels::await_reply(txn) == Ok(answer),
        "a poll the callee received expired on its old grace deadline"
    );

    // A callee that never finishes is bounded by the service deadline.
    let txn = channels::begin_call(client, 7, &request, POLL).map_err(reason)?;
    check!(
        channels::try_recv(server).map_err(reason)?.is_some(),
        "the callee did not receive the second poll"
    );
    channels::expire_deadlines(task::ticks() + channels::POLL_SERVICE_TICKS + 1);
    check!(
        channels::await_reply(txn) == Err(ChannelError::TimedOut),
        "a wedged callee held its poller past the service bound"
    );
    fresh()
}
