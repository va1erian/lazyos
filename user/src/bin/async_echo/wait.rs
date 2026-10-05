//! The `wait` op checks of `async_echo` (issue #309): a selector that parks on
//! in-flight calls and one-way endpoints together, a call that ends at its own
//! deadline while the task sleeps, more items than one wait can name, and two
//! endpoints plus a topic subscription in one wait.

use alloc::vec::Vec;

use user::central::Bus;
use user::messenger::wait::{wait_items, WaitItem, MAX_ENDPOINTS};
use user::messenger::{self, errno, Endpoint, Error};
use user::messenger_async::{Event, Selector};
use user::sys;

use super::{note, report, request, require, serve_once, text_of};

type Check = core::result::Result<(), ()>;

/// Run every check in order.
pub fn checks() -> Check {
    mixed_check()?;
    deadline_check()?;
    many_check()?;
    topic_check()
}

/// Two calls in flight plus two receives: a message wakes the step while
/// both calls are open, then the newer call is reported before the older.
fn mixed_check() -> Check {
    let (client_a, server_a) = messenger::create_pair().map_err(report)?;
    let (client_b, server_b) = messenger::create_pair().map_err(report)?;
    let (sender_c, inbox_c) = messenger::create_pair().map_err(report)?;
    let (sender_d, inbox_d) = messenger::create_pair().map_err(report)?;
    let mut selector = Selector::new();
    let older = selector
        .call(client_a, request(1, "older"), None)
        .map_err(report)?;
    let newer = selector
        .call(client_b, request(1, "newer"), None)
        .map_err(report)?;
    let recv_c = selector.recv(inbox_c);
    let recv_d = selector.recv(inbox_d);

    sender_d.send(&note("on-d")).map_err(report)?;
    let first = selector.step().map_err(report)?;
    let message_first = matches!(&first, Event::Recv { index, message }
        if *index == recv_d && text_of(&message.parcel).as_deref() == Some("on-d"));

    serve_once(server_b).map_err(report)?;
    let second = selector.step().map_err(report)?;
    let newer_first = matches!(&second, Event::Call { index, result: Ok(reply) }
        if *index == newer && text_of(reply).as_deref() == Some("newer"));

    serve_once(server_a).map_err(report)?;
    let third = selector.step().map_err(report)?;
    let older_last = matches!(third, Event::Call { index, result: Ok(_) } if index == older);

    sender_c.send(&note("on-c")).map_err(report)?;
    let fourth = selector.step().map_err(report)?;
    let last = matches!(fourth, Event::Recv { index, .. } if index == recv_c);
    let idle = selector.step().map_err(report)? == Event::Idle;
    close_all(&[
        client_a, server_a, client_b, server_b, sender_c, inbox_c, sender_d, inbox_d,
    ]);
    require(
        "WAIT:MIXED",
        message_first && newer_first && older_last && last && idle,
    )
}

/// A call nobody serves, beside an idle endpoint: the task sleeps in the wait
/// (no spinning: it returns once, after the deadline) and the call reports
/// `-ETIMEDOUT`.
fn deadline_check() -> Check {
    const TICKS: u64 = 5;
    let (client, server) = messenger::create_pair().map_err(report)?;
    let (sender, inbox) = messenger::create_pair().map_err(report)?;
    let start = sys::clock();
    let txn = client
        .begin_call(&request(1, "late"), Some(start + TICKS))
        .map_err(report)?;
    let items = [WaitItem::Endpoint(inbox), WaitItem::Call(txn)];
    let ready = wait_items(&items, 0, None).map_err(report)?;
    let slept = sys::clock() - start;
    let timed_out = matches!(client.await_reply(txn),
        Err(Error::Errno(code)) if code == -errno::ETIMEDOUT);
    close_all(&[client, server, sender, inbox]);
    require(
        "WAIT:DEADLINE",
        ready == 0b10 && timed_out && slept + 1 >= TICKS,
    )
}

/// More calls than one wait names: only the last one (outside the first
/// window) is answered, and the step still reports it.
fn many_check() -> Check {
    const CALLS: usize = MAX_ENDPOINTS + 2;
    let mut selector = Selector::new();
    let mut pairs = Vec::new();
    for _ in 0..CALLS {
        let (client, server) = messenger::create_pair().map_err(report)?;
        selector
            .call(client, request(1, "many"), None)
            .map_err(report)?;
        pairs.push((client, server));
    }
    serve_once(pairs[CALLS - 1].1).map_err(report)?;
    let event = selector.step().map_err(report)?;
    let last = matches!(event, Event::Call { index, result: Ok(_) } if index == CALLS - 1);
    // Closing the servers ends the other calls with a dead peer, each
    // reported once (the clients stay open so the transactions survive).
    for &(_, server) in &pairs {
        let _ = server.close();
    }
    let mut drained = 0;
    while let Ok(Event::Call { result: Err(_), .. }) = selector.step() {
        drained += 1;
    }
    for &(client, _) in &pairs {
        let _ = client.close();
    }
    require("WAIT:MANY", last && drained == CALLS - 1)
}

/// Two endpoints and a topic subscription's doorbell in one wait: a publish
/// wakes it on the subscription's bit only. Skipped when the image runs no
/// topics broker (no `messengerd`).
fn topic_check() -> Check {
    let Ok(mut bus) = Bus::connect() else {
        sys::write_str("ASYNC:WAIT:TOPIC:SKIP\n");
        return Ok(());
    };
    let mut subscription = bus.subscribe("demo/waitany").map_err(report)?;
    let bell = subscription.bell().map_err(report)?;
    let (sender_a, inbox_a) = messenger::create_pair().map_err(report)?;
    let (sender_b, inbox_b) = messenger::create_pair().map_err(report)?;
    bus.publish("demo/waitany", b"ping", false)
        .map_err(report)?;
    let items = [
        WaitItem::Endpoint(inbox_a),
        WaitItem::Endpoint(inbox_b),
        WaitItem::Endpoint(bell),
    ];
    let deadline = sys::clock() + 500;
    let ready = wait_items(&items, 0, Some(deadline)).map_err(report)?;
    let mut buf = [0u8; 512];
    subscription.take_ring(&mut buf);
    let event = subscription
        .recv_with(&mut buf, Some(messenger::EXPIRED_DEADLINE))
        .map_err(report)?;
    close_all(&[sender_a, inbox_a, sender_b, inbox_b]);
    let _ = subscription.unsubscribe();
    require("WAIT:TOPIC", ready == 0b100 && event.is_some())
}

/// Close every endpoint (best effort: the checks are done with them).
fn close_all(endpoints: &[Endpoint]) {
    for &endpoint in endpoints {
        let _ = endpoint.close();
    }
}
