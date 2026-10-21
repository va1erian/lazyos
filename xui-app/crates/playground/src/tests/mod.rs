//! Tests of the model modules, over the real interface table.

mod catalog_tests;
mod docs_tests;
mod form_tests;
mod gallery_run_tests;
mod gallery_tests;
mod listener_decode_tests;
mod listener_tests;
mod runner_tests;
mod show_tests;
mod snippet_tests;

use std::rc::Rc;

use rhai::{Dynamic, Map};
use rhai_lazy::msg::{codec, topics};

use crate::listener::RawEvent;

use rhai_lazy::msg::schema;
use rhai_lazy::msg::Bus;

use crate::testkit::{FakeBus, Lab};

/// A lab whose fabric serves every interface under its default name, each
/// method answering with zero values (the broker is the real fake one).
pub(crate) fn zero_lab() -> Lab {
    let bus: Rc<FakeBus> = FakeBus::new();
    for interface in schema::interfaces() {
        if bus.resolve(interface.default_service()).is_err() {
            bus.serve_zero(interface);
        }
    }
    Lab::over(bus)
}

/// A `Tick` event payload as the broker carries it.
pub(crate) fn tick_payload(unix: i64) -> Vec<u8> {
    let timed = schema::interface("os.lazy.timed.v1").unwrap();
    let mut map = Map::new();
    map.insert("unix".into(), Dynamic::from_int(unix));
    map.insert("offset".into(), Dynamic::from_int(3600));
    map.insert(
        "zone_name".into(),
        Dynamic::from("Europe/Paris".to_string()),
    );
    let body = codec::encode_payload(timed, "Tick", &Dynamic::from_map(map)).unwrap();
    topics::wrap(&body).unwrap()
}

pub(crate) fn raw(topic: &str, publisher: u64, sequence: u64, payload: Vec<u8>) -> RawEvent {
    RawEvent {
        topic: topic.to_string(),
        publisher,
        sequence,
        retained: false,
        payload,
    }
}
