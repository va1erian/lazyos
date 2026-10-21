//! A client that cannot keep up: a full event queue costs it pictures but not webd's time (the
//! back-off), a `ViewLost` it could not take is sent again, and the biggest event still fits the
//! receive buffer a client must offer.

#![cfg(not(feature = "host"))]

mod support;

use messenger_generated::os_lazy_webd_light_v1 as wire;
use support::{origin, rig, Ev};
use xui_webd::engine::EngineEvents;
use xui_webd::recording::Call;

#[test]
fn the_largest_event_fits_the_receive_buffer_a_client_must_offer() {
    use libmessenger::Header;
    let mut rig = rig();
    let app = origin(7);
    let (view, events) = rig.create_plain(app, 8, 8);
    let huge = "\u{20ac}".repeat(100_000);
    rig.queue.state(view, &huge, false, 1, &huge);
    rig.queue.link_clicked(view, &huge);
    rig.queue.clipboard_write(view, &huge);
    rig.service.pump(0);
    let sizes: Vec<usize> = rig
        .fabric
        .take_events(events)
        .into_iter()
        .map(|(_, body)| body.len())
        .collect();
    assert_eq!(sizes.len(), 3);
    // The parcel header travels with the body; the kernel drops what the buffer cannot hold.
    let header = std::mem::size_of::<Header>() + 64;
    let biggest = *sizes.iter().max().unwrap();
    assert!(
        biggest > 2 * xui_webd::limits::EVENT_TEXT_BYTES,
        "the State has two texts"
    );
    assert!(
        biggest + header <= xui_webd::limits::EVENT_BYTES_MAX,
        "{biggest}"
    );
}

#[test]
fn a_client_that_stopped_reading_costs_one_picture_per_back_off_not_one_per_paint() {
    let mut rig = rig();
    let app = origin(7);
    let (view, events) = rig.create_plain(app, 2, 2);
    let slot = rig.attach(app, view, 0, 2, 2);
    rig.fabric.set_full(events, true);
    rig.engine.take();
    rig.paint(view, 2, 2, 7);
    rig.service.pump(0);
    assert_eq!(
        *slot.borrow(),
        vec![7; 16],
        "the first copy found the queue full"
    );
    // The engine keeps painting (an animation) and the service keeps being woken: nothing is
    // copied into the slot and no whole frame is asked for.
    for now in (10..250).step_by(10) {
        rig.paint(view, 2, 2, 9);
        rig.service.pump(now);
    }
    assert_eq!(*slot.borrow(), vec![7; 16], "no copy during the back-off");
    assert!(
        rig.engine.take().is_empty(),
        "no refresh during the back-off"
    );
    assert!(rig.events(events).is_empty());
    // After the back-off, one request; asked once, not again at every turn.
    rig.service.pump(250);
    assert_eq!(rig.engine.take(), vec![Call::Refresh(view)]);
    rig.service.pump(260);
    assert!(rig.engine.take().is_empty());
    // The picture it brings meets the full queue again: another back-off, from now.
    rig.paint(view, 2, 2, 5);
    rig.service.pump(270);
    assert_eq!(*slot.borrow(), vec![5; 16]);
    rig.service.pump(400);
    assert!(rig.engine.take().is_empty());
    rig.service.pump(520);
    assert_eq!(rig.engine.take(), vec![Call::Refresh(view)]);
}

#[test]
fn a_send_that_gets_through_ends_the_back_off_early() {
    let mut rig = rig();
    let app = origin(7);
    let (view, events) = rig.create_plain(app, 2, 2);
    rig.attach(app, view, 0, 2, 2);
    rig.fabric.set_full(events, true);
    rig.paint(view, 2, 2, 7);
    rig.service.pump(0);
    rig.engine.take();
    // The client reads again; the page's next news reaches it, so the picture is worth asking for.
    rig.fabric.set_full(events, false);
    rig.queue.state(view, "back", false, 1, "");
    rig.service.pump(10);
    assert_eq!(rig.engine.take(), vec![Call::Refresh(view)]);
    rig.paint(view, 2, 2, 8);
    rig.service.pump(11);
    assert!(rig
        .events(events)
        .iter()
        .any(|event| matches!(event, Ev::Frame(_))));
}

#[test]
fn a_view_lost_while_the_client_queue_is_full_is_told_when_there_is_room() {
    let mut rig = rig();
    let app = origin(7);
    let (view, events) = rig.create_plain(app, 2, 2);
    rig.fabric.set_full(events, true);
    rig.queue.view_lost(view, xui_webd::engine::LOST_CRASHED);
    rig.service.pump(0);
    rig.service.pump(1);
    assert!(rig.events(events).is_empty());
    assert_eq!(
        rig.service.view_count(),
        1,
        "the entry stays until DestroyView"
    );
    rig.fabric.set_full(events, false);
    rig.service.pump(2);
    let lost = wire::ViewLostArgs {
        view,
        reason: wire::LOST_REASON_CRASHED,
    };
    assert_eq!(rig.events(events), vec![Ev::Lost(lost)]);
    // Told once.
    rig.service.pump(3);
    assert!(rig.events(events).is_empty());
    // The note for the host was made once too.
    let told = rig
        .service
        .take_notes()
        .into_iter()
        .filter(|note| matches!(note, xui_webd::Note::ViewLost { .. }))
        .count();
    assert_eq!(told, 1);
}

#[test]
fn a_lost_view_whose_client_died_is_dropped_not_retried() {
    let mut rig = rig();
    let app = origin(7);
    let (view, events) = rig.create_plain(app, 2, 2);
    rig.fabric.set_full(events, true);
    rig.queue.view_lost(view, xui_webd::engine::LOST_OTHER);
    rig.service.pump(0);
    rig.fabric.kill(events);
    rig.service.pump(1);
    assert_eq!(rig.service.view_count(), 0);
    assert_eq!(rig.fabric.double_closes(), 0);
}
