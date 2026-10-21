//! webd's inbox is shared and 64 messages deep, so a oneway send can meet `-EAGAIN`. What may be
//! lost is (a mouse move); what may not (a slot release, a button or key up, the visible and
//! focus states) is kept by the client and sent again at the next poll or the next message, in the
//! order it was made.

mod support;

use messenger_generated::os_lazy_webd_light_v1 as wire;
use support::{SharedWorld, TestTransport};
use xui_webd::recording::Call;
use xui_webd_client::{LightConfig, LightWebView, MouseKind, ViewEvent};

const BLUE: [u8; 4] = [0, 0, 255, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];

/// A created 2x2 view with `slots` slots, and the engine's log emptied.
fn made(slots: usize) -> (SharedWorld, LightWebView<TestTransport>, u64) {
    let world = support::world();
    let mut view =
        LightWebView::new(TestTransport::new(&world, 7), LightConfig::default()).with_slots(slots);
    view.create(2, 2).unwrap();
    let id = view.view_id().unwrap();
    world.borrow().engine.take();
    (world, view, id)
}

fn sent(world: &SharedWorld, method: u32) -> usize {
    world
        .borrow()
        .sent
        .iter()
        .filter(|(sent, _)| *sent == method)
        .count()
}

fn frames(events: &[ViewEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, ViewEvent::Frame { .. }))
        .count()
}

#[test]
fn a_slot_release_the_inbox_refused_is_sent_again_and_the_view_keeps_painting() {
    let (world, mut view, id) = made(1);
    world.borrow().paint(id, 2, 2, BLUE);
    world.borrow_mut().eagain = 1;
    assert_eq!(frames(&view.poll()), 1);
    assert_eq!(
        sent(&world, wire::METHOD_RELEASESLOT),
        0,
        "the inbox was full"
    );
    // webd still holds the only slot busy, so the next picture would have nowhere to go; the next
    // poll sends the release first and the picture then arrives.
    world.borrow().paint(id, 2, 2, WHITE);
    assert_eq!(frames(&view.poll()), 1);
    // The owed release of the first frame, and the release of the second.
    assert_eq!(sent(&world, wire::METHOD_RELEASESLOT), 2);
    // Nothing is owed any more.
    view.poll();
    assert_eq!(sent(&world, wire::METHOD_RELEASESLOT), 2);
}

#[test]
fn three_refused_releases_do_not_take_all_the_slots_out_of_use() {
    let (world, mut view, id) = made(3);
    world.borrow_mut().eagain = 1000;
    for shade in 1..=3u8 {
        world.borrow().paint(id, 2, 2, [shade, 0, 0, 255]);
        assert_eq!(frames(&view.poll()), 1);
    }
    // Every release was refused; the three slots are busy at webd.
    assert_eq!(sent(&world, wire::METHOD_RELEASESLOT), 0);
    world.borrow_mut().eagain = 0;
    world.borrow().paint(id, 2, 2, WHITE);
    assert_eq!(frames(&view.poll()), 1, "the releases went out first");
}

#[test]
fn a_key_up_the_inbox_refused_goes_out_before_the_next_message() {
    let (world, mut view, id) = made(3);
    view.key(true, 97, "a", 0);
    world.borrow_mut().eagain = 1;
    view.key(false, 97, "a", 0);
    assert_eq!(
        world.borrow().engine.calls().len(),
        1,
        "only the press so far"
    );
    view.mouse(MouseKind::Move, 0, 1, 1, 0);
    assert_eq!(
        world.borrow().engine.take(),
        vec![
            Call::Key {
                view: id,
                press: true,
                keysym: 97,
                text: "a".into(),
                mods: 0
            },
            Call::Key {
                view: id,
                press: false,
                keysym: 97,
                text: "a".into(),
                mods: 0
            },
            Call::Mouse {
                view: id,
                kind: 0,
                button: 0,
                x: 1,
                y: 1,
                mods: 0
            },
        ],
        "the key up was not overtaken by the move"
    );
}

#[test]
fn a_full_inbox_drops_moves_and_presses_but_keeps_the_button_up_for_the_next_poll() {
    let (world, mut view, id) = made(3);
    world.borrow_mut().eagain = 1000;
    view.mouse(MouseKind::Press, 0, 1, 1, 0);
    view.mouse(MouseKind::Release, 0, 1, 1, 0);
    view.mouse(MouseKind::Move, 0, 2, 2, 0);
    assert!(world.borrow().engine.calls().is_empty());
    world.borrow_mut().eagain = 0;
    view.poll();
    assert_eq!(
        world.borrow().engine.take(),
        vec![Call::Mouse {
            view: id,
            kind: 2,
            button: 0,
            x: 1,
            y: 1,
            mods: 0
        }],
        "only the release was kept"
    );
    view.poll();
    assert!(
        world.borrow().engine.calls().is_empty(),
        "and it was sent once"
    );
}

#[test]
fn only_the_last_visible_and_focus_state_is_kept() {
    let (world, mut view, id) = made(3);
    world.borrow_mut().eagain = 1000;
    view.set_visible(false);
    view.set_visible(true);
    view.focus(true);
    view.focus(false);
    world.borrow_mut().eagain = 0;
    view.poll();
    assert_eq!(
        world.borrow().engine.take(),
        vec![Call::SetVisible(id, true), Call::Focus(id, false)]
    );
}

#[test]
fn what_is_kept_is_bounded() {
    let (world, mut view, id) = made(3);
    world.borrow_mut().eagain = 100_000;
    for n in 0..500u32 {
        view.key(false, 1000 + n, "", 0);
    }
    world.borrow_mut().eagain = 0;
    view.poll();
    let keysyms: Vec<u32> = world
        .borrow()
        .engine
        .take()
        .into_iter()
        .filter_map(|call| match call {
            Call::Key { view, keysym, .. } if view == id => Some(keysym),
            _ => None,
        })
        .collect();
    assert_eq!(keysyms.len(), 64);
    assert_eq!(keysyms.last(), Some(&1499), "the newest are the ones kept");
}

#[test]
fn a_release_owed_for_a_detached_slot_is_not_sent_for_its_replacement() {
    let (world, mut view, id) = made(1);
    world.borrow().paint(id, 2, 2, BLUE);
    world.borrow_mut().eagain = 1;
    assert_eq!(frames(&view.poll()), 1);
    view.resize(4, 4).unwrap();
    view.poll();
    assert_eq!(sent(&world, wire::METHOD_RELEASESLOT), 0);
}

#[test]
fn nothing_kept_outlives_the_view() {
    let (world, mut view, _) = made(3);
    world.borrow_mut().eagain = 1000;
    view.key(false, 97, "a", 0);
    view.destroy();
    world.borrow_mut().eagain = 0;
    view.create(2, 2).unwrap();
    world.borrow().engine.take();
    view.poll();
    assert!(
        world.borrow().engine.calls().is_empty(),
        "a key up for the old view is not replayed into the new one"
    );
}

/// The receive buffer holds the largest event webd sends (a compile-time check).
const _: () = assert!(xui_webd_client::transport::EVENT_BYTES >= xui_webd::limits::EVENT_BYTES_MAX);
