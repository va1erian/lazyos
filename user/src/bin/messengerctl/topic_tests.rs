//! Topic-conformance self-test cases and payload codec helpers.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};
use user::messenger::{self, topics_client};

/// Friendly text for a Messenger API error.
fn err_text(error: messenger::Error) -> String {
    String::from(error.message())
}

/// Friendly text for a parcel codec error.
fn parcel_err_text(error: libmessenger::Error) -> String {
    String::from(error.message())
}

/// A tiny payload parcel for the self-test: one string field.
pub(crate) fn test_parcel(text: &str) -> Result<Parcel, String> {
    let mut body = Encoder::new();
    body.string(1, text).map_err(parcel_err_text)?;
    Ok(Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: 0xfeed_face,
            method: 1,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        objects: Vec::new(),
    })
}

/// The first string field of an event payload.
fn payload_text(event: &topics_client::Event) -> Result<String, String> {
    let parcel = event.parcel().map_err(err_text)?;
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(parcel_err_text)? {
        if field.kind == Kind::String {
            return Ok(String::from(field.as_str().map_err(parcel_err_text)?));
        }
    }
    Err(String::from("payload has no string field"))
}

/// Two subscriptions on one topic both receive the same event.
pub(crate) fn selftest_fanout(client: &topics_client::Client) -> Result<(), String> {
    let first = client
        .subscribe("topics/fanout", topics_client::Qos::Latest)
        .map_err(err_text)?;
    let second = client
        .subscribe("topics/fanout", topics_client::Qos::Buffered(4))
        .map_err(err_text)?;
    let payload = test_parcel("fanout-1")?;
    let matched = client
        .publish("topics/fanout", &payload)
        .map_err(err_text)?;
    if matched != 2 {
        return Err(format!("matched {matched}, expected 2"));
    }
    let a = first
        .next_event(None)
        .map_err(err_text)?
        .ok_or("first subscriber got no event")?;
    let b = second
        .next_event(None)
        .map_err(err_text)?
        .ok_or("second subscriber got no event")?;
    if payload_text(&a)? != "fanout-1" || payload_text(&b)? != "fanout-1" {
        return Err(String::from("fanout payloads differ"));
    }
    if a.sequence != b.sequence {
        return Err(String::from("fanout copies disagree on sequence"));
    }
    first.unsubscribe().map_err(err_text)?;
    second.unsubscribe().map_err(err_text)?;
    Ok(())
}

/// `+` matches exactly one segment, trailing `#` matches zero or more.
///
/// The checks run under a private `selftest/` root: the platform services
/// publish real topics under `system/` and the broker is shared, so a
/// `system/#` subscription would race their traffic (issue #169).
pub(crate) fn selftest_wildcard(client: &topics_client::Client) -> Result<(), String> {
    let one = client
        .subscribe("selftest/+/up", topics_client::Qos::Latest)
        .map_err(err_text)?;
    let any = client
        .subscribe("selftest/#", topics_client::Qos::Buffered(8))
        .map_err(err_text)?;

    // Four segments: `selftest/+/up` must not match, `selftest/#` must.
    let deep = test_parcel("network-up")?;
    let matched = client
        .publish("selftest/events/network/up", &deep)
        .map_err(err_text)?;
    if matched != 1 {
        return Err(format!("deep publish matched {matched}, expected 1"));
    }
    // Three segments: both filters match.
    let shallow = test_parcel("events-up")?;
    let matched = client
        .publish("selftest/events/up", &shallow)
        .map_err(err_text)?;
    if matched != 2 {
        return Err(format!("shallow publish matched {matched}, expected 2"));
    }
    // One segment: only `selftest/#` matches; `#` stands for zero segments too.
    let root = test_parcel("selftest-up")?;
    let matched = client.publish("selftest", &root).map_err(err_text)?;
    if matched != 1 {
        return Err(format!("root publish matched {matched}, expected 1"));
    }

    let event = one
        .next_event(None)
        .map_err(err_text)?
        .ok_or("`selftest/+/up` got no event")?;
    if event.topic != "selftest/events/up" || payload_text(&event)? != "events-up" {
        return Err(format!(
            "`selftest/+/up` received {} ({})",
            event.topic,
            payload_text(&event)?
        ));
    }
    if one.poll_event().map_err(err_text)?.is_some() {
        return Err(String::from("`selftest/+/up` matched a second event"));
    }

    let topics = [
        "selftest/events/network/up",
        "selftest/events/up",
        "selftest",
    ];
    for expected in topics {
        let event = any
            .next_event(None)
            .map_err(err_text)?
            .ok_or("`selftest/#` queue ran dry")?;
        if event.topic != expected {
            return Err(format!(
                "`selftest/#` got {} expected {expected}",
                event.topic
            ));
        }
    }
    one.unsubscribe().map_err(err_text)?;
    any.unsubscribe().map_err(err_text)?;
    Ok(())
}

/// A retained publish is replayed to a later subscriber.
pub(crate) fn selftest_retained(client: &topics_client::Client) -> Result<(), String> {
    let payload = test_parcel("netd-up")?;
    let matched = client
        .publish_retained("selftest/health/netd", &payload)
        .map_err(err_text)?;
    if matched != 0 {
        return Err(format!("retained publish matched {matched}, expected 0"));
    }
    let subscription = client
        .subscribe("selftest/health/netd", topics_client::Qos::Latest)
        .map_err(err_text)?;
    let event = subscription
        .next_event(None)
        .map_err(err_text)?
        .ok_or("new subscriber got no retained value")?;
    if !event.retained {
        return Err(String::from("replayed event is not marked retained"));
    }
    if event.topic != "selftest/health/netd" || payload_text(&event)? != "netd-up" {
        return Err(String::from("retained value payload changed"));
    }
    subscription.unsubscribe().map_err(err_text)?;
    Ok(())
}

/// `buffered(1)` drops the oldest event on overflow and counts it.
pub(crate) fn selftest_drop(client: &topics_client::Client) -> Result<(), String> {
    let subscription = client
        .subscribe("topics/drop", topics_client::Qos::Buffered(1))
        .map_err(err_text)?;
    for text in ["drop-1", "drop-2", "drop-3"] {
        let payload = test_parcel(text)?;
        client.publish("topics/drop", &payload).map_err(err_text)?;
    }
    let stats = subscription.stats().map_err(err_text)?;
    if stats.drops != 2 {
        return Err(format!("drops {} expected 2", stats.drops));
    }
    if stats.queued != 1 {
        return Err(format!("queued {} expected 1", stats.queued));
    }
    let event = subscription
        .next_event(None)
        .map_err(err_text)?
        .ok_or("dropping subscriber got no event")?;
    if payload_text(&event)? != "drop-3" {
        return Err(format!(
            "oldest was not dropped: got {}",
            payload_text(&event)?
        ));
    }
    subscription.unsubscribe().map_err(err_text)?;
    Ok(())
}

/// `reliable` redelivers the unacked head and retires it on `ack`; `conflate`
/// coalesces a publisher's pending events and counts the replacement.
pub(crate) fn selftest_qos(client: &topics_client::Client) -> Result<(), String> {
    let reliable = client
        .subscribe("topics/reliable", topics_client::Qos::Reliable)
        .map_err(err_text)?;
    for text in ["rel-1", "rel-2"] {
        let payload = test_parcel(text)?;
        client
            .publish("topics/reliable", &payload)
            .map_err(err_text)?;
    }
    let first = reliable
        .next_event(None)
        .map_err(err_text)?
        .ok_or("reliable queue empty")?;
    let retry = reliable
        .next_event(None)
        .map_err(err_text)?
        .ok_or("reliable retry empty")?;
    if retry.sequence != first.sequence || payload_text(&retry)? != "rel-1" {
        return Err(String::from("reliable did not redeliver the head"));
    }
    reliable.ack(first.sequence).map_err(err_text)?;
    let second = reliable
        .next_event(None)
        .map_err(err_text)?
        .ok_or("reliable second event missing")?;
    if payload_text(&second)? != "rel-2" {
        return Err(String::from("reliable did not advance after ack"));
    }
    reliable.ack(second.sequence).map_err(err_text)?;
    reliable.unsubscribe().map_err(err_text)?;

    let conflate = client
        .subscribe("topics/conflate", topics_client::Qos::Conflate)
        .map_err(err_text)?;
    for text in ["conf-1", "conf-2"] {
        let payload = test_parcel(text)?;
        client
            .publish("topics/conflate", &payload)
            .map_err(err_text)?;
    }
    let stats = conflate.stats().map_err(err_text)?;
    if stats.drops != 1 {
        return Err(format!("conflate drops {} expected 1", stats.drops));
    }
    let event = conflate
        .next_event(None)
        .map_err(err_text)?
        .ok_or("conflate queue empty")?;
    if payload_text(&event)? != "conf-2" {
        return Err(String::from("conflate kept the older event"));
    }
    conflate.unsubscribe().map_err(err_text)?;
    Ok(())
}

/// After `unsubscribe` later publishes no longer match.
pub(crate) fn selftest_unsubscribe(client: &topics_client::Client) -> Result<(), String> {
    let subscription = client
        .subscribe("topics/unsub", topics_client::Qos::Latest)
        .map_err(err_text)?;
    subscription.unsubscribe().map_err(err_text)?;
    let payload = test_parcel("gone")?;
    let matched = client.publish("topics/unsub", &payload).map_err(err_text)?;
    if matched != 0 {
        return Err(format!("unsubscribed publish matched {matched}"));
    }
    Ok(())
}
