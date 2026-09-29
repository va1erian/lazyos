//! Topic browser commands: `topics` and `tail <filter> [count]`.

use alloc::format;
use alloc::string::String;
use libmessenger::{Decoder, Kind};
use user::messenger::topics_client;
use user::sys;

use super::commands::report;

/// `topics`: list the topics the broker has seen, with subscriber counts and
/// whether a retained value is held.
pub(crate) fn print_topics() {
    let client = match topics_client::Client::connect() {
        Ok(client) => client,
        Err(error) => return report(error.message()),
    };
    match client.list() {
        Ok(entries) if entries.is_empty() => sys::write_str("topics: none seen\n"),
        Ok(entries) => {
            sys::write_str(&format!("topics: {} known\n", entries.len()));
            for entry in &entries {
                let retained = if entry.retained {
                    "retained"
                } else {
                    "volatile"
                };
                sys::write_str(&format!(
                    "  {}  subs {}  {retained}\n",
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
    let client = match topics_client::Client::connect() {
        Ok(client) => client,
        Err(error) => return report(error.message()),
    };
    let subscription = match client.subscribe(filter, topics_client::Qos::Latest) {
        Ok(subscription) => subscription,
        Err(error) => return report(error.message()),
    };
    sys::write_str(&format!("tail {filter}: {count} event(s)\n"));
    for _ in 0..count {
        match subscription.next_event(None) {
            Ok(Some(event)) => {
                let retained = if event.retained { " retained" } else { "" };
                sys::write_str(&format!(
                    "[{}] {} seq {}{} from slot {}: {}\n",
                    event.topic,
                    event.topic,
                    event.sequence,
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
fn describe_payload(event: &topics_client::Event) -> String {
    if let Ok(parcel) = event.parcel() {
        let mut decoder = Decoder::new(&parcel.body);
        while let Ok(Some(field)) = decoder.next() {
            if field.kind == Kind::String {
                if let Ok(text) = field.as_str() {
                    return String::from(text);
                }
            }
        }
    }
    format!("{} payload byte(s)", event.payload.len())
}
