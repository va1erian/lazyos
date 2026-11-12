//! Topic browser commands: `topics` and `tail <filter> [count]`.

use alloc::format;
use alloc::string::String;
use libmessenger::client::Deadline;
use libmessenger::{Decoder, Kind};
use user::central::{Bus, Event};
use user::sys;

use super::commands::report;

/// `topics`: list the topics the broker has seen, with subscriber counts and
/// whether a retained value is held.
pub(crate) fn print_topics() {
    let mut bus = match Bus::connect() {
        Ok(bus) => bus,
        Err(error) => return report(error.message()),
    };
    match bus.list() {
        Ok(entries) if entries.is_empty() => sys::write_str("topics: none seen\n"),
        Ok(entries) => {
            sys::write_str(&format!("topics: {} known\n", entries.len()));
            for entry in &entries {
                let retained = if entry.retained {
                    "retained"
                } else {
                    "volatile"
                };
                // The payload type comes from the generated declaration table
                // (`idl/*.midl`, issue #307): a concrete topic is matched
                // against the declared patterns.
                let payload = messenger_generated::declared_topic(&entry.topic)
                    .map(|decl| decl.payload)
                    .unwrap_or("-");
                sys::write_str(&format!(
                    "  {}  subs {}  {retained}  payload {payload}\n",
                    entry.topic, entry.subscribers
                ));
            }
        }
        Err(error) => report(error.message()),
    }
}

/// `tail <filter> [count]`: subscribe with `latest` QoS and print up to
/// `count` events (default 5, capped at 64). The task blocks between events;
/// another task's publishes wake it through the broker's deferred reply.
pub(crate) fn tail(rest: &str) {
    let mut parts = rest.split_whitespace();
    let Some(filter) = parts.next() else {
        return report("usage: tail <filter> [count]");
    };
    let count: usize = match parts.next() {
        Some(text) => match text.parse() {
            Ok(value) => core::cmp::min(value, 64usize),
            Err(_) => return report("usage: tail <filter> [count]"),
        },
        None => 5,
    };
    let mut bus = match Bus::connect() {
        Ok(bus) => bus,
        Err(error) => return report(error.message()),
    };
    let mut subscription = match bus.subscribe(filter) {
        Ok(subscription) => subscription,
        Err(error) => return report(error.message()),
    };
    // A doorbell, though `recv` does not need one: a `tail` stopped with
    // Ctrl-C never unsubscribes, and the broker reaps a subscription only
    // when its bell's other end is gone. Without it every stopped `tail`
    // would hold one of its app's subscriptions until logout.
    let _ = subscription.bell();
    sys::write_str(&format!("tail {filter}: {count} event(s)\n"));
    for _ in 0..count {
        match subscription.recv(Deadline::Never) {
            Ok(Some(event)) => {
                let retained = if event.retained { " retained" } else { "" };
                sys::write_str(&format!(
                    "[{}] {} seq {}{} from slot {}: {}\n",
                    event.topic,
                    event.topic,
                    event.seq,
                    retained,
                    event.publisher,
                    describe_payload(&event)
                ));
            }
            Ok(None) => {
                report("timed out waiting for an event");
                break;
            }
            Err(error) => {
                report(error.message());
                break;
            }
        }
    }
    let _ = subscription.unsubscribe();
}

/// A readable one-line summary of an event payload: the first string field of
/// the publisher's parcel, or its size when the payload is not text.
fn describe_payload(event: &Event) -> String {
    let mut decoder = Decoder::new(&event.payload);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::String {
            if let Ok(text) = field.as_str() {
                return String::from(text);
            }
        }
    }
    format!("{} payload byte(s)", event.payload.len())
}
