//! Clients cannot touch each other's views, groups are the service's, and failure on either side
//! (a client that dies, an engine that loses a view or is gone) ends up where it should.

#![cfg(not(feature = "host"))]

mod support;

use messenger_generated::os_lazy_webd_light_v1 as wire;
use support::{origin, rig, Ev};
use xui_webd::engine::{EngineEvents, LOST_CRASHED, LOST_MEMORY, LOST_OTHER, LOST_REFUSED};
use xui_webd::fabric::errno::*;
use xui_webd::recording::Call;

#[test]
fn a_view_is_unusable_by_another_client_and_looks_like_one_that_does_not_exist() {
    let mut rig = rig();
    let (alice, bob) = (origin(7), origin(8));
    let (view, alice_events) = rig.create_plain(alice, 4, 4);
    rig.attach(alice, view, 0, 4, 4);
    rig.engine.take();
    let before = rig.fabric.open_handles();

    // Every call Bob can make on Alice's view is refused as "no such view", the same as for a
    // number nobody has.
    let (handle, _) = rig.fabric.new_buffer(64);
    assert_eq!(rig.attach_handle(bob, view, 1, handle, 64), Err(ENOENT));
    assert_eq!(
        rig.attach_handle(bob, view + 1000, 1, handle, 64),
        Err(ENOENT)
    );
    assert_eq!(rig.resize(bob, view, 8, 8), Err(ENOENT));
    assert_eq!(rig.load_html(bob, view, b"<script>", ""), Err(ENOENT));
    for method_body in [
        (
            wire::METHOD_SETVISIBLE,
            wire::encode_set_visible_args(&wire::SetVisibleArgs {
                view,
                visible: false,
            })
            .unwrap(),
        ),
        (
            wire::METHOD_REFRESH,
            wire::encode_refresh_args(&wire::RefreshArgs { view }).unwrap(),
        ),
        (
            wire::METHOD_RELEASESLOT,
            wire::encode_release_slot_args(&wire::ReleaseSlotArgs { view, slot: 0 }).unwrap(),
        ),
        (
            wire::METHOD_FOCUS,
            wire::encode_focus_args(&wire::FocusArgs {
                view,
                focused: true,
            })
            .unwrap(),
        ),
        (
            wire::METHOD_MOUSE,
            wire::encode_mouse_args(&wire::MouseArgs {
                view,
                kind: 0,
                button: 0,
                x: 1,
                y: 1,
                mods: 0,
            })
            .unwrap(),
        ),
        (
            wire::METHOD_WHEEL,
            wire::encode_wheel_args(&wire::WheelArgs {
                view,
                x: 0,
                y: 0,
                ticks_x: 1,
                ticks_y: 1,
                mods: 0,
            })
            .unwrap(),
        ),
        (
            wire::METHOD_KEY,
            wire::encode_key_args(&wire::KeyArgs {
                view,
                press: true,
                keysym: 97,
                text: "a".into(),
                mods: 0,
            })
            .unwrap(),
        ),
    ] {
        assert_eq!(
            rig.raw(bob, method_body.0, method_body.1, vec![]),
            Err(ENOENT)
        );
    }
    // Destroying is "fine" for him too, and does nothing.
    assert!(rig.destroy(bob, view).is_ok());
    assert_eq!(rig.engine.take(), vec![], "the engine heard nothing of it");
    // Bob's refused buffers were closed; Alice's view and endpoint are intact.
    assert_eq!(rig.fabric.open_handles(), before);
    assert!(rig.fabric.channel_open(alice_events));
    rig.paint(view, 4, 4, 5);
    rig.service.pump(0);
    assert!(rig
        .events(alice_events)
        .iter()
        .any(|ev| matches!(ev, Ev::Frame(_))));
    assert!(rig.resize(alice, view, 8, 8).is_ok());
}

#[test]
fn an_unlabelled_caller_is_nobodys_labelled_client_and_unlabelled_callers_do_not_share() {
    let mut rig = rig();
    let package = origin(7);
    let script_a = support::origin(0);
    let mut script_b = origin(0);
    script_b.sender += 1;
    let (view, _) = rig.create_plain(package, 2, 2);
    let (script_view, _) = rig.create_plain(script_a, 2, 2);
    assert_eq!(rig.resize(script_a, view, 4, 4), Err(ENOENT));
    assert_eq!(rig.resize(package, script_view, 4, 4), Err(ENOENT));
    assert_eq!(rig.resize(script_b, script_view, 4, 4), Err(ENOENT));
    assert!(rig.resize(script_a, script_view, 4, 4).is_ok());
    // The same package from another task is the same client.
    let mut same_package = package;
    same_package.sender += 50;
    assert!(rig.resize(same_package, view, 4, 4).is_ok());
    // Another user running the same package is another client.
    let mut other_user = package;
    other_user.uid = 1001;
    assert_eq!(rig.resize(other_user, view, 8, 8), Err(ENOENT));
}

#[test]
fn web_process_groups_belong_to_the_service_one_per_client() {
    let mut rig = rig();
    let (alice, bob) = (origin(7), origin(8));
    let (a1, _) = rig.create_plain(alice, 2, 2);
    let (a2, _) = rig.create_plain(alice, 2, 2);
    let (b1, _) = rig.create_plain(bob, 2, 2);
    let (a3, _) = rig.create_plain(alice, 2, 2);
    let group_of = |rig: &support::Rig, view: u64| {
        rig.engine
            .calls()
            .into_iter()
            .find_map(|call| match call {
                Call::ViewCreate(id, config) if id == view => Some(config.group),
                _ => None,
            })
            .unwrap()
    };
    let (ga, gb) = (group_of(&rig, a1), group_of(&rig, b1));
    assert!(
        ga > 0 && gb > 0,
        "a group of 0 would mean a process of its own"
    );
    assert_ne!(ga, gb, "two clients never share a web process");
    assert_eq!(group_of(&rig, a2), ga);
    assert_eq!(group_of(&rig, a3), ga, "one client's views do");
    // The number survives the client's views coming and going.
    for view in [a1, a2, a3] {
        rig.destroy(alice, view).unwrap();
    }
    let (a4, _) = rig.create_plain(alice, 2, 2);
    assert_eq!(group_of(&rig, a4), ga);
    // An unlabelled script gets a number of its own, never a package's.
    let (s1, _) = rig.create_plain(origin(0), 2, 2);
    assert!(![ga, gb].contains(&group_of(&rig, s1)));
}

#[test]
fn a_client_that_dies_loses_its_views_and_everything_it_held() {
    let mut rig = rig();
    let (alice, bob) = (origin(7), origin(8));
    let (a, a_events) = rig.create_plain(alice, 2, 2);
    let (a2, a2_events) = rig.create_plain(alice, 2, 2);
    let (b, b_events) = rig.create_plain(bob, 2, 2);
    rig.attach(alice, a, 0, 2, 2);
    rig.attach(alice, a2, 0, 2, 2);
    rig.attach(bob, b, 0, 2, 2);
    rig.service.pump(0);
    rig.events(a_events);
    rig.events(a2_events);
    rig.events(b_events);
    rig.engine.take();

    // Alice's task exits: both her endpoints are closed by the kernel.
    rig.fabric.kill(a_events);
    rig.fabric.kill(a2_events);
    // The next ping finds out, destroys her views in the engine and frees her slots.
    rig.service.pump(1000);
    let calls = rig.engine.take();
    assert!(calls.contains(&Call::ViewDestroy(a)) && calls.contains(&Call::ViewDestroy(a2)));
    assert!(!calls.contains(&Call::ViewDestroy(b)));
    assert_eq!(rig.service.view_count(), 1);
    assert!(!rig.fabric.channel_open(a_events) && !rig.fabric.channel_open(a2_events));
    assert!(rig.fabric.channel_open(b_events));
    // Bob is untouched and her ids are dead for everyone.
    assert_eq!(rig.events(b_events), vec![Ev::Ping]);
    assert_eq!(rig.resize(alice, a, 4, 4), Err(ENOENT));
    rig.destroy(bob, b).unwrap();
    assert_eq!(rig.fabric.open_handles(), 0);
    assert_eq!(rig.fabric.double_closes(), 0);
}

#[test]
fn a_client_found_dead_while_the_engine_paints_loses_the_view_then_too() {
    let mut rig = rig();
    let app = origin(7);
    let (view, events) = rig.create_plain(app, 2, 2);
    rig.attach(app, view, 0, 2, 2);
    rig.fabric.kill(events);
    rig.paint(view, 2, 2, 1);
    rig.service.pump(0);
    assert_eq!(rig.engine.creates_and_destroys(view), (1, 1));
    assert_eq!(rig.service.view_count(), 0);
    assert_eq!(rig.fabric.open_handles(), 0);
}

#[test]
fn a_view_the_engine_loses_reaches_the_client_and_answers_with_enodev_until_destroyed() {
    let mut rig = rig();
    let app = origin(7);
    let (view, events) = rig.create_plain(app, 2, 2);
    rig.attach(app, view, 0, 2, 2);
    rig.queue.state(view, "x", false, 1, "");
    rig.paint(view, 2, 2, 1);
    rig.queue.view_lost(view, LOST_CRASHED);
    rig.service.pump(0);
    // What the dead view said last is not delivered; the loss is, once.
    let sent = rig.events(events);
    assert_eq!(
        sent.iter()
            .filter(|ev| !matches!(ev, Ev::Ping))
            .collect::<Vec<_>>(),
        vec![&Ev::Lost(wire::ViewLostArgs {
            view,
            reason: wire::LOST_REASON_CRASHED
        })]
    );
    rig.service.pump(5000);
    rig.paint(view, 2, 2, 2);
    rig.service.pump(5001);
    assert_eq!(
        rig.events(events),
        vec![Ev::Ping],
        "nothing but the liveness probe follows"
    );

    assert_eq!(rig.resize(app, view, 4, 4), Err(ENODEV));
    assert_eq!(rig.load_html(app, view, b"<p>", ""), Err(ENODEV));
    let (outcome, _) = rig.attach_bytes(app, view, 1, 16);
    assert_eq!(outcome, Err(ENODEV));
    // The place is still taken, and the entry goes only when the client says so; the engine already
    // dropped the id, so it is not asked to destroy it again.
    assert_eq!(rig.service.view_count(), 1);
    rig.destroy(app, view).unwrap();
    assert_eq!(rig.service.view_count(), 0);
    assert_eq!(rig.engine.creates_and_destroys(view), (1, 0));
    assert_eq!(rig.fabric.open_handles(), 0);
}

#[test]
fn every_engine_reason_maps_to_a_lost_reason() {
    let mut rig = rig();
    let app = origin(7);
    let mut seen = Vec::new();
    for (engine_reason, wanted) in [
        (LOST_REFUSED, wire::LOST_REASON_REFUSED),
        (LOST_CRASHED, wire::LOST_REASON_CRASHED),
        (LOST_MEMORY, wire::LOST_REASON_OUT_OF_MEMORY),
        (LOST_OTHER, wire::LOST_REASON_OTHER),
        (99, wire::LOST_REASON_UNKNOWN),
    ] {
        let (view, events) = rig.create_plain(app, 2, 2);
        rig.queue.view_lost(view, engine_reason);
        rig.service.pump(0);
        seen.push((
            rig.events(events).into_iter().find_map(|ev| match ev {
                Ev::Lost(lost) => Some(lost.reason),
                _ => None,
            }),
            wanted,
        ));
        rig.destroy(app, view).unwrap();
    }
    for (got, wanted) in seen {
        assert_eq!(got, Some(wanted));
    }
}

#[test]
fn when_the_engine_is_gone_every_view_is_lost_once_and_the_service_can_shut_down() {
    let mut rig = rig();
    let (alice, bob) = (origin(7), origin(8));
    let (a, a_events) = rig.create_plain(alice, 2, 2);
    let (b, b_events) = rig.create_plain(bob, 2, 2);
    rig.attach(alice, a, 0, 2, 2);
    rig.service.engine_lost();
    rig.service.engine_lost();
    for (events, view) in [(a_events, a), (b_events, b)] {
        assert_eq!(
            rig.events(events),
            vec![Ev::Lost(wire::ViewLostArgs {
                view,
                reason: wire::LOST_REASON_OTHER
            })]
        );
    }
    rig.service.shutdown();
    assert_eq!(rig.service.view_count(), 0);
    assert_eq!(rig.fabric.open_handles(), 0);
    assert_eq!(rig.fabric.double_closes(), 0);
}

#[test]
fn shutdown_destroys_live_views_in_the_engine() {
    let mut rig = rig();
    let (view, _) = rig.create_plain(origin(7), 2, 2);
    rig.engine.take();
    rig.service.shutdown();
    assert_eq!(rig.engine.take(), vec![Call::ViewDestroy(view)]);
}
