//! What each client holds in webd (`Service::client_usage`): the views and the slot memory, which
//! is the part of webd's memory that belongs to its clients.

#![cfg(not(feature = "host"))]

mod support;

use support::{origin, rig};

#[test]
fn an_empty_service_holds_nothing() {
    assert!(rig().service.client_usage().is_empty());
}

#[test]
fn each_client_is_charged_its_own_views_and_slots() {
    let mut rig = rig();
    let (docs, mail) = (origin(7), origin(8));
    let (first, _) = rig.create_plain(docs, 4, 4);
    rig.create_plain(docs, 4, 4);
    let (other, _) = rig.create_plain(mail, 8, 2);
    rig.attach(docs, first, 0, 4, 4);
    rig.attach(docs, first, 1, 4, 4);
    rig.attach(mail, other, 0, 8, 2);

    let usage = rig.service.client_usage();
    assert_eq!(usage.len(), 2, "one entry per client");
    let of = |label: u32| {
        usage
            .iter()
            .find(|entry| entry.client == origin(label).client())
            .copied()
            .expect("the client is listed")
    };
    assert_eq!((of(7).views, of(7).slot_bytes), (2, 2 * 4 * 4 * 4));
    assert_eq!((of(8).views, of(8).slot_bytes), (1, 8 * 2 * 4));
    // What the table says is what the per-client queries say.
    assert_eq!(rig.service.client_views(docs.client()), of(7).views);
}

#[test]
fn a_destroyed_view_gives_its_slots_back() {
    let mut rig = rig();
    let app = origin(7);
    let (view, _) = rig.create_plain(app, 4, 4);
    rig.attach(app, view, 0, 4, 4);
    assert_eq!(rig.service.client_usage()[0].slot_bytes, 4 * 4 * 4);
    rig.destroy(app, view).unwrap();
    assert!(rig.service.client_usage().is_empty());
}
