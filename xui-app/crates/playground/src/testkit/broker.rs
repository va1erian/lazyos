//! The topic broker of the test fabric: `os.lazy.messenger.topics.v1` with the
//! real broker's QoS rules (`user/src/bin/messengerd/broker.rs`): latest keeps
//! one event, buffered `depth`, conflate the newest per publisher, reliable
//! eight and redelivers its head until acked; retained values are replayed to
//! a new subscription.

use std::collections::VecDeque;
use std::rc::Rc;

use rhai::{Blob, Dynamic, Map};
use rhai_lazy::msg::schema;

use super::bus::{FakeBus, Owner, Reply};

/// Depth of a reliable queue and of the conflate window.
pub const FIXED_DEPTH: usize = 8;

#[derive(Clone, Debug)]
pub struct Ev {
    pub topic: String,
    pub publisher: u64,
    pub sequence: u64,
    pub retained: bool,
    pub payload: Vec<u8>,
}

#[derive(Default)]
struct Sub {
    id: u64,
    filter: String,
    qos: u32,
    depth: u32,
    queue: VecDeque<Ev>,
    conflated: Vec<Ev>,
    matched: u64,
    delivered: u64,
    drops: u64,
}

impl Sub {
    fn enqueue(&mut self, ev: Ev) {
        self.matched += 1;
        match self.qos {
            0 => {
                if !self.queue.is_empty() {
                    self.queue.clear();
                    self.drops += 1;
                }
                self.queue.push_back(ev);
            }
            1 => {
                while self.queue.len() >= self.depth.max(1) as usize {
                    self.queue.pop_front();
                    self.drops += 1;
                }
                self.queue.push_back(ev);
            }
            3 => {
                while self.queue.len() >= FIXED_DEPTH {
                    self.queue.pop_front();
                    self.drops += 1;
                }
                self.queue.push_back(ev);
            }
            _ => {
                if let Some(slot) = self
                    .conflated
                    .iter_mut()
                    .find(|e| e.publisher == ev.publisher)
                {
                    *slot = ev;
                    self.drops += 1;
                } else {
                    while self.conflated.len() >= FIXED_DEPTH {
                        self.conflated.remove(0);
                        self.drops += 1;
                    }
                    self.conflated.push(ev);
                }
            }
        }
    }

    fn head(&self) -> Option<&Ev> {
        if self.qos == 2 {
            self.conflated.first()
        } else {
            self.queue.front()
        }
    }

    fn queued(&self) -> u64 {
        (self.queue.len() + self.conflated.len()) as u64
    }

    /// The head reached the subscriber: retire it unless reliable.
    fn commit(&mut self) {
        self.delivered += 1;
        match self.qos {
            3 => {}
            2 => {
                if !self.conflated.is_empty() {
                    self.conflated.remove(0);
                }
            }
            _ => {
                self.queue.pop_front();
            }
        }
    }
}

/// The broker's state.
#[derive(Default)]
pub struct Broker {
    subs: Vec<Sub>,
    retained: Vec<Ev>,
    seen: Vec<String>,
    sequence: u64,
    next_id: u64,
}

impl Broker {
    /// Publish `payload` (already in the broker's envelope) as `publisher`;
    /// returns how many subscriptions it reached.
    pub fn publish(
        &mut self,
        topic: &str,
        payload: Vec<u8>,
        retained: bool,
        publisher: u64,
    ) -> u64 {
        self.sequence += 1;
        let ev = Ev {
            topic: topic.to_string(),
            publisher,
            sequence: self.sequence,
            retained,
            payload,
        };
        if !self.seen.iter().any(|t| t == topic) {
            self.seen.push(topic.to_string());
        }
        if retained {
            self.retained.retain(|e| e.topic != topic);
            self.retained.push(ev.clone());
        }
        let mut reached = 0;
        for sub in &mut self.subs {
            if schema::topic_matches(&sub.filter, topic) {
                sub.enqueue(ev.clone());
                reached += 1;
            }
        }
        reached
    }

    fn subscribe(&mut self, filter: &str, qos: u32, depth: u32) -> u64 {
        self.next_id += 1;
        let mut sub = Sub {
            id: self.next_id,
            filter: filter.to_string(),
            qos,
            depth: depth.clamp(1, 64),
            ..Sub::default()
        };
        for ev in self
            .retained
            .iter()
            .filter(|e| schema::topic_matches(filter, &e.topic))
        {
            sub.enqueue(ev.clone());
        }
        let id = sub.id;
        self.subs.push(sub);
        id
    }
}

fn event_map(ev: &Ev) -> Map {
    let mut map = Map::new();
    map.insert("topic".into(), Dynamic::from(ev.topic.clone()));
    map.insert("publisher".into(), Dynamic::from_int(ev.publisher as i64));
    map.insert("sequence".into(), Dynamic::from_int(ev.sequence as i64));
    map.insert("retained".into(), Dynamic::from_bool(ev.retained));
    map.insert(
        "payload".into(),
        Dynamic::from_blob(ev.payload.clone() as Blob),
    );
    map
}

fn int(map: &Map, key: &str) -> i64 {
    map.get(key).and_then(|v| v.as_int().ok()).unwrap_or(0)
}

fn text(map: &Map, key: &str) -> String {
    map.get(key).map(|v| v.to_string()).unwrap_or_default()
}

/// Install the broker as `os.lazy.messenger.topics`.
pub fn install(bus: &Rc<FakeBus>) {
    bus.serve(
        "os.lazy.messenger.topics",
        "os.lazy.messenger.topics.v1",
        Owner::default(),
        |bus, req| -> Reply {
            let mut broker = bus.broker.borrow_mut();
            let mut out = Map::new();
            match req.method.name {
                "Publish" => {
                    let payload = req
                        .args
                        .get("payload")
                        .and_then(|p| p.clone().try_cast::<Blob>())
                        .unwrap_or_default();
                    let topic = text(&req.args, "topic");
                    let retained = req
                        .args
                        .get("retained")
                        .and_then(|r| r.as_bool().ok())
                        .unwrap_or(false);
                    let n = broker.publish(&topic, payload, retained, 7);
                    out.insert("matched".into(), Dynamic::from_int(n as i64));
                }
                "Subscribe" => {
                    let id = broker.subscribe(
                        &text(&req.args, "filter"),
                        int(&req.args, "qos") as u32,
                        int(&req.args, "depth") as u32,
                    );
                    out.insert("subscription".into(), Dynamic::from_int(id as i64));
                }
                "NextEvent" => {
                    let id = int(&req.args, "subscription") as u64;
                    let Some(sub) = broker.subs.iter_mut().find(|s| s.id == id) else {
                        return Err((2, "no such subscription".into()));
                    };
                    match sub.head().cloned() {
                        Some(ev) => {
                            sub.commit();
                            out.insert("event".into(), Dynamic::from_map(event_map(&ev)));
                        }
                        None => {
                            drop(broker);
                            // 110: nothing arrived; 125: the scenario is spent.
                            return Err(if bus.idle() {
                                (110, String::new())
                            } else {
                                (125, String::new())
                            });
                        }
                    }
                }
                "Ack" => {
                    let id = int(&req.args, "subscription") as u64;
                    let upto = int(&req.args, "sequence") as u64;
                    if let Some(sub) = broker.subs.iter_mut().find(|s| s.id == id) {
                        while sub.queue.front().is_some_and(|e| e.sequence <= upto) {
                            sub.queue.pop_front();
                        }
                    }
                }
                "Unsubscribe" => {
                    let id = int(&req.args, "subscription") as u64;
                    broker.subs.retain(|s| s.id != id);
                }
                "ListTopics" => {
                    let rows: Vec<Dynamic> = broker
                        .seen
                        .iter()
                        .map(|t| {
                            let mut row = Map::new();
                            row.insert("topic".into(), Dynamic::from(t.clone()));
                            let n = broker
                                .subs
                                .iter()
                                .filter(|s| schema::topic_matches(&s.filter, t))
                                .count();
                            row.insert("subscribers".into(), Dynamic::from_int(n as i64));
                            let ret = broker.retained.iter().any(|e| &e.topic == t);
                            row.insert("retained".into(), Dynamic::from_bool(ret));
                            Dynamic::from_map(row)
                        })
                        .collect();
                    out.insert("topics".into(), Dynamic::from_array(rows));
                }
                "Stats" => {
                    let id = int(&req.args, "subscription") as u64;
                    let Some(sub) = broker.subs.iter().find(|s| s.id == id) else {
                        return Err((2, "no such subscription".into()));
                    };
                    let mut stats = Map::new();
                    stats.insert("qos".into(), Dynamic::from_int(sub.qos.into()));
                    stats.insert("depth".into(), Dynamic::from_int(sub.depth.into()));
                    stats.insert("queued".into(), Dynamic::from_int(sub.queued() as i64));
                    stats.insert("delivered".into(), Dynamic::from_int(sub.delivered as i64));
                    stats.insert("matched".into(), Dynamic::from_int(sub.matched as i64));
                    stats.insert("drops".into(), Dynamic::from_int(sub.drops as i64));
                    out.insert("stats".into(), Dynamic::from_map(stats));
                }
                _ => {}
            }
            Ok(out)
        },
    );
}
