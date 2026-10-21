//! End to end over the in-memory fabric and the recording engine: a client creates a view, attaches
//! slots, loads a page, and sees what the engine paints and reports arrive as events in the slots
//! it gave. The refusals (quotas, bad input) are in `quotas.rs`, the isolation and failure
//! cases in `isolation.rs`.

// With the `host` feature the library wants the C++ half's symbols at link time, which a test
// program does not have; the `ffi` unit tests stand in for it (`cargo test --lib --features host`).
#![cfg(not(feature = "host"))]

mod support;

use messenger_generated::os_lazy_webd_light_v1 as wire;
use support::{origin, rig, Ev};
use xui_webd::engine::{EngineConfig, EngineEvents};
use xui_webd::recording::Call;

#[test]
fn create_attach_load_paint_release_and_the_second_frame_goes_to_the_next_slot() {
    let mut rig = rig();
    let app = origin(7);
    let (view, events) = rig.create_plain(app, 4, 2);
    assert!(view > 0);
    let slots: Vec<_> = (0..3)
        .map(|slot| rig.attach(app, view, slot, 4, 2))
        .collect();

    // The engine got a light view of the size, in the client's group, with nothing widened.
    let group = rig.service.group_of(origin(7).client()).unwrap();
    assert_eq!(
        rig.engine.calls()[0],
        Call::ViewCreate(
            view,
            EngineConfig {
                width: 4,
                height: 2,
                group,
                ..EngineConfig::default()
            }
        )
    );
    // Attaching with nothing painted yet asks for a first picture.
    assert_eq!(rig.engine.calls().last(), Some(&Call::Refresh(view)));

    rig.load_html(app, view, b"<p>hello</p>", "about:blank")
        .unwrap();
    assert_eq!(
        rig.engine.calls().last(),
        Some(&Call::LoadHtml {
            view,
            html: b"<p>hello</p>".to_vec(),
            base_url: "about:blank".into()
        })
    );

    // The engine reports the page and paints it.
    rig.queue.state(view, "Hello", false, 1, "");
    rig.paint(view, 4, 2, 0x11);
    rig.service.pump(0);
    let sent = rig.events(events);
    assert!(sent.contains(&Ev::State(wire::StateArgs {
        view,
        title: "Hello".into(),
        loading: false,
        finished: 1,
        error: String::new()
    })));
    assert!(sent.contains(&Ev::Frame(wire::FrameArgs {
        view,
        slot: 0,
        seq: 1,
        width: 4,
        height: 2
    })));
    assert_eq!(*slots[0].borrow(), vec![0x11; 32], "the frame is in slot 0");
    assert_eq!(*slots[1].borrow(), vec![0; 32]);

    // Slot 0 is busy until released, so the next frame lands in slot 1, then 2.
    rig.paint(view, 4, 2, 0x22);
    rig.service.pump(0);
    assert_eq!(
        rig.events(events),
        vec![Ev::Frame(wire::FrameArgs {
            view,
            slot: 1,
            seq: 2,
            width: 4,
            height: 2
        })]
    );
    rig.paint(view, 4, 2, 0x33);
    rig.service.pump(0);
    assert_eq!(rig.events(events).len(), 1);
    assert_eq!(*slots[1].borrow(), vec![0x22; 32]);
    assert_eq!(*slots[2].borrow(), vec![0x33; 32]);

    // Every slot is busy: the frame is dropped, never queued, and slot 0 is not touched.
    rig.engine.take();
    rig.paint(view, 4, 2, 0x44);
    rig.service.pump(0);
    assert!(rig.events(events).is_empty());
    assert_eq!(*slots[0].borrow(), vec![0x11; 32]);
    // Releasing a slot asks the engine for a fresh whole frame, which then fills it.
    rig.release(app, view, 0).unwrap();
    assert_eq!(rig.engine.take(), vec![Call::Refresh(view)]);
    rig.paint(view, 4, 2, 0x55);
    rig.service.pump(0);
    assert_eq!(
        rig.events(events),
        vec![Ev::Frame(wire::FrameArgs {
            view,
            slot: 0,
            seq: 4,
            width: 4,
            height: 2
        })]
    );
    assert_eq!(
        *slots[0].borrow(),
        vec![0x55; 32],
        "the newest picture, not the dropped one"
    );
}

#[test]
fn the_pages_news_reaches_the_client() {
    let mut rig = rig();
    let app = origin(7);
    let (view, events) = rig.create_plain(app, 8, 8);
    rig.queue.scroll(view, 0, 120, true);
    rig.queue.link_clicked(view, "https://example.com/next");
    rig.queue.clipboard_write(view, "copied text");
    rig.service.pump(0);
    let sent = rig.events(events);
    assert!(sent.contains(&Ev::Scroll(wire::ScrollArgs {
        view,
        x: 0,
        y: 120,
        at_bottom: true
    })));
    assert!(sent.contains(&Ev::Link(wire::LinkClickedArgs {
        view,
        url: "https://example.com/next".into()
    })));
    assert!(sent.contains(&Ev::Clipboard(wire::ClipboardWriteArgs {
        view,
        text: "copied text".into()
    })));
}

#[test]
fn long_texts_are_cut_and_news_about_a_view_nobody_has_is_ignored() {
    let mut rig = rig();
    let app = origin(7);
    let (view, events) = rig.create_plain(app, 8, 8);
    rig.queue.state(view, &"é".repeat(100_000), false, 1, "");
    rig.queue.link_clicked(9999, "https://example.com/");
    rig.service.pump(0);
    let sent = rig.events(events);
    let Some(Ev::State(state)) = sent.iter().find(|ev| matches!(ev, Ev::State(_))) else {
        panic!("no state event");
    };
    assert_eq!(state.title.len(), xui_webd::limits::EVENT_TEXT_BYTES);
    assert!(state.title.chars().all(|c| c == 'é'));
    assert_eq!(sent.len(), 1, "only the state; view 9999 is not a view");
}

#[test]
fn input_goes_to_the_engine_with_the_modifier_bits_it_knows() {
    let mut rig = rig();
    let app = origin(7);
    let (view, _) = rig.create_plain(app, 8, 8);
    rig.engine.take();
    let send = |rig: &mut support::Rig, method, body| rig.raw(app, method, body, vec![]).unwrap();
    send(
        &mut rig,
        wire::METHOD_MOUSE,
        wire::encode_mouse_args(&wire::MouseArgs {
            view,
            kind: 1,
            button: 2,
            x: 3,
            y: 4,
            mods: 0xFF,
        })
        .unwrap(),
    );
    send(
        &mut rig,
        wire::METHOD_WHEEL,
        wire::encode_wheel_args(&wire::WheelArgs {
            view,
            x: 1,
            y: 2,
            ticks_x: 0,
            ticks_y: -3,
            mods: 1,
        })
        .unwrap(),
    );
    send(
        &mut rig,
        wire::METHOD_KEY,
        wire::encode_key_args(&wire::KeyArgs {
            view,
            press: true,
            keysym: 97,
            text: "a".into(),
            mods: 2,
        })
        .unwrap(),
    );
    send(
        &mut rig,
        wire::METHOD_FOCUS,
        wire::encode_focus_args(&wire::FocusArgs {
            view,
            focused: true,
        })
        .unwrap(),
    );
    send(
        &mut rig,
        wire::METHOD_SETVISIBLE,
        wire::encode_set_visible_args(&wire::SetVisibleArgs {
            view,
            visible: false,
        })
        .unwrap(),
    );
    assert_eq!(
        rig.engine.take(),
        vec![
            Call::Mouse {
                view,
                kind: 1,
                button: 2,
                x: 3,
                y: 4,
                mods: 0xF
            },
            Call::Wheel {
                view,
                x: 1,
                y: 2,
                ticks_x: 0,
                ticks_y: -3,
                mods: 1
            },
            Call::Key {
                view,
                press: true,
                keysym: 97,
                text: "a".into(),
                mods: 2
            },
            Call::Focus(view, true),
            Call::SetVisible(view, false),
        ]
    );
}

#[test]
fn resize_detaches_the_slots_and_a_new_attach_asks_for_a_picture_of_the_new_size() {
    let mut rig = rig();
    let app = origin(7);
    let (view, events) = rig.create_plain(app, 4, 2);
    rig.attach(app, view, 0, 4, 2);
    rig.paint(view, 4, 2, 1);
    rig.service.pump(0);
    assert_eq!(rig.events(events).len(), 1);

    rig.resize(app, view, 8, 4).unwrap();
    assert_eq!(rig.engine.calls().last(), Some(&Call::Resize(view, 8, 4)));
    // A frame painted at the old size, or with no slot attached, goes nowhere.
    rig.paint(view, 4, 2, 2);
    rig.service.pump(0);
    rig.paint(view, 8, 4, 3);
    rig.service.pump(0);
    assert!(rig.events(events).is_empty());
    // The old slot's size is no longer right; a slot of the new size is accepted.
    let (old, _) = rig.attach_bytes(app, view, 0, 4 * 2 * 4);
    assert!(old.is_err(), "too small for 8x4");
    rig.engine.take();
    let slot = rig.attach(app, view, 0, 8, 4);
    assert_eq!(rig.engine.take(), vec![Call::Refresh(view)]);
    rig.paint(view, 8, 4, 4);
    rig.service.pump(0);
    assert_eq!(
        rig.events(events),
        vec![Ev::Frame(wire::FrameArgs {
            view,
            slot: 0,
            seq: 2,
            width: 8,
            height: 4
        })]
    );
    assert_eq!(*slot.borrow(), vec![4; 8 * 4 * 4]);
}

#[test]
fn a_busy_slot_cannot_be_replaced_and_a_bigger_buffer_is_fine() {
    let mut rig = rig();
    let app = origin(7);
    let (view, events) = rig.create_plain(app, 2, 2);
    rig.attach(app, view, 0, 2, 2);
    rig.paint(view, 2, 2, 9);
    rig.service.pump(0);
    assert_eq!(rig.events(events).len(), 1);
    let (busy, _) = rig.attach_bytes(app, view, 0, 16);
    assert_eq!(busy, Err(xui_webd::fabric::errno::EBUSY));
    rig.release(app, view, 0).unwrap();
    let (again, _) = rig.attach_bytes(app, view, 0, 1000);
    assert!(again.is_ok());
    assert_eq!(rig.fabric.double_closes(), 0);
}

#[test]
fn the_pings_come_once_a_second_and_a_destroyed_view_frees_everything() {
    let mut rig = rig();
    let app = origin(7);
    let (view, events) = rig.create_plain(app, 2, 2);
    rig.attach(app, view, 0, 2, 2);
    rig.service.pump(0);
    rig.service.pump(500);
    assert!(
        rig.events(events).is_empty(),
        "the first turn starts the clock"
    );
    rig.service.pump(1000);
    assert_eq!(rig.events(events), vec![Ev::Ping]);
    rig.service.pump(1999);
    assert!(rig.events(events).is_empty());
    rig.service.pump(2000);
    assert_eq!(rig.events(events), vec![Ev::Ping]);

    rig.destroy(app, view).unwrap();
    assert_eq!(rig.engine.creates_and_destroys(view), (1, 1));
    assert_eq!(rig.service.view_count(), 0);
    assert_eq!(
        rig.fabric.open_handles(),
        0,
        "the endpoint and the slot are closed"
    );
    assert_eq!(rig.fabric.double_closes(), 0);
    // Destroying again, or a view that never was, is not an error.
    assert!(rig.destroy(app, view).is_ok());
    assert!(rig.destroy(app, 12345).is_ok());
    // And the ids are not reused.
    let (second, _) = rig.create_plain(app, 2, 2);
    assert!(second > view);
}

#[test]
fn a_full_client_queue_loses_the_frame_but_not_the_slot_and_the_picture_is_asked_for_again() {
    let mut rig = rig();
    let app = origin(7);
    let (view, events) = rig.create_plain(app, 2, 2);
    let slot = rig.attach(app, view, 0, 2, 2);
    rig.fabric.set_full(events, true);
    rig.engine.take();
    rig.paint(view, 2, 2, 7);
    rig.service.pump(0);
    assert!(rig.events(events).is_empty());
    // The slot is still free; the picture is asked for again once the back-off is over.
    assert!(rig.engine.take().is_empty(), "not at the paint rate");
    rig.fabric.set_full(events, false);
    rig.service.pump(300);
    assert_eq!(rig.engine.take(), vec![Call::Refresh(view)]);
    rig.paint(view, 2, 2, 8);
    rig.service.pump(301);
    assert_eq!(
        rig.events(events),
        vec![Ev::Frame(wire::FrameArgs {
            view,
            slot: 0,
            seq: 1,
            width: 2,
            height: 2
        })]
    );
    assert_eq!(*slot.borrow(), vec![8; 16]);
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
    assert_eq!(*slot.borrow(), vec![7; 16], "the first copy found the queue full");
    // The engine keeps painting (an animation) and the service keeps being woken: nothing is
    // copied into the slot and no whole frame is asked for.
    for now in (10..250).step_by(10) {
        rig.paint(view, 2, 2, 9);
        rig.service.pump(now);
    }
    assert_eq!(*slot.borrow(), vec![7; 16], "no copy during the back-off");
    assert!(rig.engine.take().is_empty(), "no refresh during the back-off");
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
    assert_eq!(rig.service.view_count(), 1, "the entry stays until DestroyView");
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

#[test]
fn the_notes_say_what_happened_for_the_host_to_trace() {
    use xui_webd::Note;
    let mut rig = rig();
    let app = origin(7);
    let (view, _) = rig.create_plain(app, 2, 2);
    rig.queue.say("ENGINE:UP");
    rig.service.pump(0);
    rig.paint(view, 2, 2, 1);
    rig.service.pump(0);
    rig.destroy(app, view).unwrap();
    let notes = rig.service.take_notes();
    assert!(
        matches!(notes[0], Note::ViewCreated { view: v, width: 2, height: 2, .. } if v == view)
    );
    assert!(notes.contains(&Note::Engine("ENGINE:UP".into())));
    assert!(notes.contains(&Note::FrameDropped {
        view,
        why: "no free slot"
    }));
    assert!(notes.contains(&Note::ViewEnded {
        view,
        why: "destroyed"
    }));
    assert!(rig.service.take_notes().is_empty());
}
