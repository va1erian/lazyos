//! What the list shows for one event, and who published it: the registry
//! snapshot a publisher's task slot is resolved against.

use rhai::{Dynamic, Map};

use crate::listener::RawEvent;
use crate::listener_decode::{decode, Body};
use crate::live::LiveJoin;
use crate::show;

/// A registry snapshot older than this is not used to name a publisher: task
/// slots are reused, so an old answer could be someone else.
pub const MAX_NAME_AGE_MS: u64 = 3_000;
/// Longest one-line summary of an event.
const SUMMARY: usize = 160;

/// Who owns which task slot, as of one registry read.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    taken_ms: u64,
    owners: Vec<(u64, String)>,
}

impl Registry {
    /// The owners of `join` at `taken_ms`. A slot owning two names is left
    /// out: it names nobody.
    pub fn from_join(join: &LiveJoin, taken_ms: u64) -> Registry {
        let mut owners: Vec<(u64, String)> = Vec::new();
        for service in &join.services {
            let slot = service.row.owner;
            if join.owner_name(slot).is_some() && !owners.iter().any(|(s, _)| *s == slot) {
                owners.push((slot, service.row.name.clone()));
            }
        }
        Registry { taken_ms, owners }
    }

    /// The registered name that owns `slot` as of `now_ms`, if the snapshot
    /// is fresh and exactly one name owns it. Never a stale guess.
    pub fn resolve(&self, slot: u64, now_ms: u64) -> Option<&str> {
        if now_ms.saturating_sub(self.taken_ms) > MAX_NAME_AGE_MS {
            return None;
        }
        self.owners
            .iter()
            .find(|(s, _)| *s == slot)
            .map(|(_, name)| name.as_str())
    }
}

/// One event in the list.
#[derive(Debug, Clone)]
pub struct Event {
    /// Local number, in arrival order.
    pub id: u64,
    pub at_ms: u64,
    pub topic: String,
    pub publisher: u64,
    /// The name that owned the publisher's slot when the event arrived.
    pub publisher_name: Option<String>,
    pub sequence: u64,
    pub retained: bool,
    pub body: Body,
    pub(crate) search: String,
}

fn flatten(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(ch);
        if out.chars().count() >= max {
            out.push_str("...");
            break;
        }
    }
    out
}

impl Event {
    pub(crate) fn new(id: u64, at_ms: u64, raw: RawEvent, name: Option<String>) -> Event {
        let body = decode(&raw.topic, &raw.payload);
        let mut event = Event {
            id,
            at_ms,
            topic: raw.topic,
            publisher: raw.publisher,
            publisher_name: name,
            sequence: raw.sequence,
            retained: raw.retained,
            body,
            search: String::new(),
        };
        event.search = format!(
            "{} {} {}",
            event.topic,
            event.publisher_name.as_deref().unwrap_or(""),
            event.summary()
        )
        .to_lowercase();
        event
    }

    /// The publisher as the list shows it: the owning name, or `?`.
    pub fn publisher_text(&self) -> String {
        self.publisher_name
            .clone()
            .unwrap_or_else(|| "?".to_string())
    }

    /// The payload on one line.
    pub fn summary(&self) -> String {
        match &self.body {
            Body::Decoded { value, .. } => flatten(&show::to_rhai(value), SUMMARY),
            Body::Bytes { .. } => self.body.tree(),
            Body::Undecodable { error, .. } => flatten(error, SUMMARY),
        }
    }

    /// The expanded value tree.
    pub fn tree(&self) -> String {
        self.body.tree()
    }

    /// "Copy as Rhai": the event as the map a script's `sub.next()` returns.
    pub fn to_rhai(&self) -> String {
        let mut map = Map::new();
        map.insert("topic".into(), Dynamic::from(self.topic.clone()));
        map.insert("publisher".into(), Dynamic::from_int(self.publisher as i64));
        map.insert("sequence".into(), Dynamic::from_int(self.sequence as i64));
        map.insert("retained".into(), Dynamic::from_bool(self.retained));
        map.insert("payload".into(), self.body.value());
        show::to_rhai(&Dynamic::from_map(map))
    }
}
