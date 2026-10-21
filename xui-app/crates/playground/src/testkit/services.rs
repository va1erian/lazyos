//! The services the Gallery's examples talk to, as the in-memory fabric
//! serves them: just enough of each to make the example's output
//! deterministic, built on the same schema codec the real calls use.

use std::cell::RefCell;
use std::rc::Rc;

use rhai::{Array, Dynamic, Map};
use rhai_lazy::msg::{codec, schema, topics};

use super::bus::{FakeBus, Owner, Reply};

/// The fabric and a log of what the services were asked to do.
pub struct World {
    pub bus: Rc<FakeBus>,
    /// One line per interesting call, `service:detail`.
    pub log: Rc<RefCell<Vec<String>>>,
}

impl World {
    /// The log lines starting with `prefix`.
    pub fn logged(&self, prefix: &str) -> Vec<String> {
        self.log
            .borrow()
            .iter()
            .filter(|l| l.starts_with(prefix))
            .cloned()
            .collect()
    }
}

fn str_of(map: &Map, key: &str) -> String {
    map.get(key).map(|v| v.to_string()).unwrap_or_default()
}

fn int_of(map: &Map, key: &str) -> i64 {
    map.get(key).and_then(|v| v.as_int().ok()).unwrap_or(-1)
}

fn map(pairs: Vec<(&str, Dynamic)>) -> Map {
    pairs.into_iter().map(|(k, v)| (k.into(), v)).collect()
}

/// Publish `value` (a map or enum name) on `topic` as `publisher`, encoded
/// with the topic's declared type, retained as declared.
pub fn publish_declared(bus: &FakeBus, topic: &str, publisher: u64, value: &Dynamic) -> u64 {
    let (interface, decl) = schema::declared_topic(topic).expect("a declared topic");
    let body = codec::encode_payload(interface, decl.payload, value).expect("encode payload");
    let wrapped = topics::wrap(&body).expect("wrap");
    bus.broker
        .borrow_mut()
        .publish(topic, wrapped, decl.retained, publisher)
}

/// Publish raw bytes on an undeclared topic.
pub fn publish_bytes(bus: &FakeBus, topic: &str, publisher: u64, bytes: &[u8]) -> u64 {
    let wrapped = topics::wrap(bytes).expect("wrap");
    bus.broker
        .borrow_mut()
        .publish(topic, wrapped, false, publisher)
}

fn registry(bus: &FakeBus) {
    bus.serve(
        "os.lazy.messenger.registry",
        "os.lazy.messenger.registry.v1",
        Owner::default(),
        |bus, req| -> Reply {
            let mut out = Map::new();
            if req.method.name == "List" {
                let rows: Array = bus
                    .registry_rows()
                    .into_iter()
                    .map(|r| {
                        Dynamic::from_map(map(vec![
                            ("name", Dynamic::from(r.name)),
                            ("object", Dynamic::from_int(r.object as i64)),
                            ("owner", Dynamic::from_int(r.owner as i64)),
                            (
                                "interfaces",
                                Dynamic::from_array(
                                    r.interfaces
                                        .iter()
                                        .map(|i| Dynamic::from_int(*i as i64))
                                        .collect(),
                                ),
                            ),
                            (
                                "lease_remaining",
                                Dynamic::from_int(r.lease_remaining as i64),
                            ),
                            ("owner_uid", Dynamic::from_int(r.owner_uid as i64)),
                            ("owner_label", Dynamic::from_int(r.owner_label as i64)),
                            ("owner_session", Dynamic::from_int(r.owner_session as i64)),
                        ]))
                    })
                    .collect();
                out.insert("entries".into(), Dynamic::from_array(rows));
            }
            Ok(out)
        },
    );
}

/// The fabric of the Gallery's examples, with the retained values the
/// examples read already published.
pub fn gallery_world() -> World {
    let bus = FakeBus::new();
    let log: Rc<RefCell<Vec<String>>> = Rc::default();
    registry(&bus);

    // A user's app that owns a name but implements no known interface.
    bus.serve_external(
        "zeta.app",
        &[],
        Owner {
            uid: 1000,
            label: 7,
            session: 1,
        },
    );

    bus.serve(
        "os.lazy.timed",
        "os.lazy.timed.v1",
        Owner::default(),
        |_, _| Ok(Map::new()),
    );
    publish_declared(
        &bus,
        "time/tick",
        12,
        &Dynamic::from_map(map(vec![
            ("unix", Dynamic::from_int(1_700_000_000)),
            ("offset", Dynamic::from_int(3600)),
            ("zone_name", Dynamic::from("Europe/Paris".to_string())),
        ])),
    );

    bus.serve(
        "os.lazy.confd",
        "os.lazy.confd.v1",
        Owner::default(),
        |_, req| {
            let mut out = Map::new();
            if req.method.name == "Get" && str_of(&req.args, "path") == "sys/ui/theme" {
                out.insert(
                    "value".into(),
                    Dynamic::from_map(map(vec![
                        ("kind", Dynamic::from_int(3)),
                        ("str_value", Dynamic::from("dark".to_string())),
                    ])),
                );
            }
            Ok(out)
        },
    );

    bus.serve(
        "os.lazy.sysmond",
        "os.lazy.sysmond.v1",
        Owner::default(),
        |_, _| Ok(Map::new()),
    );
    publish_declared(
        &bus,
        "system/stats/memory",
        13,
        &Dynamic::from_map(map(vec![
            ("heap_used", Dynamic::from_int(1024 * 1024)),
            ("heap_total", Dynamic::from_int(4 * 1024 * 1024)),
        ])),
    );

    bus.serve(
        "os.lazy.healthd",
        "os.lazy.healthd.v1",
        Owner::default(),
        |_, _| Ok(Map::new()),
    );
    publish_declared(
        &bus,
        "system/health/summary",
        14,
        &Dynamic::from_map(map(vec![
            ("name", Dynamic::from("summary".to_string())),
            ("status", Dynamic::from("ok".to_string())),
            ("detail", Dynamic::from("all services up".to_string())),
            ("tick", Dynamic::from_int(42)),
        ])),
    );

    let opened = log.clone();
    bus.serve(
        "os.lazy.mimed",
        "os.lazy.mimed.v1",
        Owner::default(),
        move |_, req| {
            let mut out = Map::new();
            let path = str_of(&req.args, "path");
            match req.method.name {
                "Guess" => {
                    let mime = if path.ends_with(".pdf") {
                        "application/pdf"
                    } else {
                        "text/plain"
                    };
                    out.insert("mime".into(), Dynamic::from(mime.to_string()));
                }
                "Verbs" => {
                    out.insert(
                        "verbs".into(),
                        Dynamic::from_array(vec![Dynamic::from("open".to_string())]),
                    );
                }
                "Lookup" => {
                    out.insert("app".into(), Dynamic::from("os.lazy.pdf".to_string()));
                }
                "Open" => {
                    opened
                        .borrow_mut()
                        .push(format!("mimed:open {path} {}", str_of(&req.args, "verb")));
                    out.insert("app".into(), Dynamic::from("os.lazy.pdf".to_string()));
                    out.insert("mime".into(), Dynamic::from("application/pdf".to_string()));
                    out.insert("published".into(), Dynamic::from_bool(true));
                    out.insert("launched".into(), Dynamic::from_bool(true));
                }
                _ => {}
            }
            Ok(out)
        },
    );

    let toasts = log.clone();
    let next_toast = std::cell::Cell::new(1i64);
    bus.serve(
        "os.lazy.notify",
        "os.lazy.notify.v1",
        Owner::default(),
        move |_, req| {
            let mut out = Map::new();
            if req.method.name == "Notify" {
                let replaces = req.args.get("replaces_id").and_then(|v| v.as_int().ok());
                let id = replaces.unwrap_or_else(|| {
                    let id = next_toast.get();
                    next_toast.set(id + 1);
                    id
                });
                let progress = req
                    .args
                    .get("progress")
                    .map_or(-1, |_| int_of(&req.args, "progress"));
                toasts.borrow_mut().push(format!(
                    "notify:{}:{}:{}:progress={progress}:id={id}",
                    str_of(&req.args, "level"),
                    str_of(&req.args, "title"),
                    str_of(&req.args, "body"),
                ));
                out.insert("id".into(), Dynamic::from_int(id));
            }
            Ok(out)
        },
    );

    let mixer = log.clone();
    let master = std::cell::Cell::new(32768i64);
    bus.serve(
        "os.lazy.audio.mixer",
        "os.lazy.audio.mixer.v1",
        Owner::default(),
        move |_, req| {
            let mut out = Map::new();
            match req.method.name {
                "GetMaster" => {
                    out.insert(
                        "master".into(),
                        Dynamic::from_map(map(vec![
                            ("gain_q16", Dynamic::from_int(master.get())),
                            ("mute", Dynamic::from_bool(false)),
                            ("card", Dynamic::from_bool(true)),
                        ])),
                    );
                }
                "SetMaster" => {
                    master.set(int_of(&req.args, "gain_q16"));
                    mixer
                        .borrow_mut()
                        .push(format!("mixer:set_master {}", master.get()));
                }
                _ => {}
            }
            Ok(out)
        },
    );

    bus.serve(
        "os.lazy.init",
        "os.lazy.init.v1",
        Owner::default(),
        |_, _| Ok(Map::new()),
    );

    let asked = log.clone();
    bus.serve(
        "os.lazy.elevd",
        "os.lazy.elevd.v1",
        Owner::default(),
        move |_, req| {
            let mut out = Map::new();
            if req.method.name == "Request" {
                asked.borrow_mut().push(format!(
                    "elevd:request {} {}",
                    str_of(&req.args, "operation"),
                    req.args
                        .get("args")
                        .map(|a| a.to_string())
                        .unwrap_or_default()
                ));
                out.insert("detail".into(), Dynamic::from("restarted".to_string()));
            }
            Ok(out)
        },
    );

    World { bus, log }
}

/// A Rust implementation of the pirate echo, for the example that calls it.
pub fn add_pirate(bus: &FakeBus) {
    bus.serve(
        "demo.pirate",
        "os.lazy.echo.v1",
        Owner::default(),
        |_, req| {
            let mut out = Map::new();
            match req.method.name {
                "Echo" => {
                    let text = str_of(&req.args, "text").to_uppercase();
                    let n = int_of(&req.args, "count").max(0) as usize;
                    out.insert(
                        "reply".into(),
                        Dynamic::from(format!("{text}! ARR. ").repeat(n)),
                    );
                }
                "Ping" => {
                    out.insert("alive".into(), Dynamic::from_bool(true));
                }
                _ => {}
            }
            Ok(out)
        },
    );
}
