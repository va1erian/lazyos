//! `LightWebView` against the real service half over its in-memory fabric and a recording engine:
//! a view is created, painted, resized, clicked, lost and made again, and what the client does with
//! a service that refuses, vanishes or is slower than the page.

mod support;

use messenger_generated::os_lazy_webd_light_v1 as wire;
use support::{view, TestTransport};
use xui_webd::engine::EngineEvents;
use xui_webd::recording::Call;
use xui_webd_client::{Colour, Error, LightConfig, LightWebView, Lost, MouseKind, ViewEvent};

const BLUE: [u8; 4] = [0, 0, 255, 255];
const RED: [u8; 4] = [255, 0, 0, 255];

fn frames(events: &[ViewEvent]) -> Vec<(u64, u32, u32)> {
    events
        .iter()
        .filter_map(|event| match event {
            ViewEvent::Frame { seq, width, height } => Some((*seq, *width, *height)),
            _ => None,
        })
        .collect()
}

#[test]
fn create_load_paint_and_the_frame_is_copied_out_and_released() {
    let (world, mut view) = view(7);
    view.create(4, 2).unwrap();
    let id = view.view_id().unwrap();
    assert!(view.is_live());

    // The engine got a light view of the size, and the three slots started free.
    {
        let world = world.borrow();
        assert!(matches!(
            &world.engine.calls()[0],
            Call::ViewCreate(view, config) if *view == id && config.width == 4 && config.height == 2
        ));
        assert_eq!(world.service.view_count(), 1);
    }

    view.set_html("<p>hello</p>", "about:blank").unwrap();
    assert!(world.borrow().engine.calls().iter().any(|call| matches!(
        call,
        Call::LoadHtml { view, html, base_url } if *view == id && html == b"<p>hello</p>" && base_url == "about:blank"
    )));

    {
        let world = world.borrow();
        world.queue.state(id, "Hello", false, 1, "");
        world.paint(id, 4, 2, BLUE);
    }
    let events = view.poll();
    assert_eq!(frames(&events), vec![(1, 4, 2)]);
    assert!(events.iter().any(|event| matches!(
        event,
        ViewEvent::State(state) if state.title == "Hello" && state.finished == 1 && !state.loading
    )));
    assert_eq!(view.page().title, "Hello");
    let picture = view.picture();
    assert_eq!(
        (picture.width(), picture.height(), picture.seq()),
        (4, 2, 1)
    );
    assert_eq!(picture.rgba(), BLUE.repeat(8).as_slice());
    assert_eq!(picture.dominant(), Colour::Blue);

    // The slot went back: the next frame is painted without a refresh in between.
    let released: Vec<_> = world
        .borrow()
        .sent
        .iter()
        .filter(|(method, _)| *method == wire::METHOD_RELEASESLOT)
        .cloned()
        .collect();
    assert_eq!(released.len(), 1);
    {
        let world = world.borrow();
        world.paint(id, 4, 2, RED);
    }
    assert_eq!(frames(&view.poll()), vec![(2, 4, 2)]);
    assert_eq!(view.picture().dominant(), Colour::Red);
    assert_eq!(view.counters().applied, 2);
}

#[test]
fn a_client_that_does_not_poll_loses_frames_and_ends_on_the_newest_picture() {
    let (world, mut view) = view(7);
    view.create(4, 2).unwrap();
    let id = view.view_id().unwrap();

    // Five pictures while the client reads nothing: three slots fill, two frames are dropped.
    for shade in 1..=5u8 {
        let mut world = world.borrow_mut();
        world.paint(id, 4, 2, [shade, 0, 0, 255]);
        world.pump();
    }
    let events = view.poll();
    assert_eq!(
        frames(&events),
        vec![(3, 4, 2)],
        "only the newest of the three is shown"
    );
    assert_eq!(view.picture().rgba()[0], 3);
    assert_eq!(view.counters().skipped, 2);

    // Releasing the slots made the service ask the engine for a whole new frame.
    let refreshes = world
        .borrow()
        .engine
        .calls()
        .iter()
        .filter(|call| matches!(call, Call::Refresh(v) if *v == id))
        .count();
    assert!(refreshes >= 1, "the dropped frames are asked for again");
    world.borrow().paint(id, 4, 2, [5, 0, 0, 255]);
    assert_eq!(frames(&view.poll()), vec![(4, 4, 2)]);
    assert_eq!(
        view.picture().rgba()[0],
        5,
        "the picture is the newest, not the dropped ones"
    );
}

#[test]
fn resize_attaches_slots_of_the_new_size_and_a_stale_frame_is_not_shown() {
    let (world, mut view) = view(7);
    view.create(4, 2).unwrap();
    let id = view.view_id().unwrap();
    world.borrow().paint(id, 4, 2, BLUE);
    view.poll();

    view.resize(8, 4).unwrap();
    assert_eq!(view.size(), (8, 4));
    assert!(world
        .borrow()
        .engine
        .calls()
        .contains(&Call::Resize(id, 8, 4)));
    // The picture stays at its own size until the first frame of the new one.
    assert_eq!((view.picture().width(), view.picture().height()), (4, 2));

    // A frame event of the old size that was already on its way is ignored, not copied from a slot
    // that holds nothing.
    let channel = view.events_handle().unwrap();
    let stale = wire::encode_frame_args(&wire::FrameArgs {
        view: id,
        slot: 0,
        seq: 50,
        width: 4,
        height: 2,
    })
    .unwrap();
    world
        .borrow_mut()
        .inject(channel, wire::METHOD_FRAME, stale);
    assert!(frames(&view.poll()).is_empty());
    assert_eq!(view.counters().ignored, 1);
    assert_eq!(view.picture().dominant(), Colour::Blue);

    world.borrow().paint(id, 8, 4, RED);
    assert_eq!(frames(&view.poll()), vec![(2, 8, 4)]);
    assert_eq!(view.picture().rgba().len(), 8 * 4 * 4);
    assert_eq!(view.picture().dominant(), Colour::Red);

    // The same size again asks for nothing.
    world.borrow().engine.take();
    view.resize(8, 4).unwrap();
    assert!(world.borrow().engine.calls().is_empty());
}

#[test]
fn the_pages_news_reaches_the_app() {
    let (world, mut view) = view(7);
    view.create(8, 8).unwrap();
    let id = view.view_id().unwrap();
    {
        let world = world.borrow();
        world.queue.scroll(id, 0, 120, true);
        world.queue.link_clicked(id, "https://example.com/next");
        world.queue.clipboard_write(id, "copied text");
        world.queue.state(id, "T", true, 0, "");
    }
    let events = view.poll();
    assert!(events.contains(&ViewEvent::Scroll {
        x: 0,
        y: 120,
        at_bottom: true
    }));
    assert!(events.contains(&ViewEvent::LinkClicked("https://example.com/next".into())));
    assert!(events.contains(&ViewEvent::ClipboardWrite("copied text".into())));
    assert_eq!(view.scroll(), (0, 120, true));
    assert!(view.page().loading);
    assert_eq!(view.page().title, "T");
}

#[test]
fn input_visibility_and_focus_reach_the_engine() {
    let (world, mut view) = view(7);
    view.create(8, 8).unwrap();
    let id = view.view_id().unwrap();
    world.borrow().engine.take();
    view.mouse(MouseKind::Press, 2, 3, 4, 0xF);
    view.mouse(MouseKind::Move, 0, 5, 6, 0);
    view.mouse(MouseKind::Release, 0, 5, 6, 0);
    view.wheel(1, 2, 0, -3, 1);
    view.key(true, 97, "a", 2);
    view.focus(true);
    view.set_visible(false);
    view.refresh();
    let mouse = |kind, button, x, y, mods| Call::Mouse {
        view: id,
        kind,
        button,
        x,
        y,
        mods,
    };
    assert_eq!(
        world.borrow().engine.take(),
        vec![
            mouse(1, 2, 3, 4, 0xF),
            mouse(0, 0, 5, 6, 0),
            mouse(2, 0, 5, 6, 0),
            Call::Wheel {
                view: id,
                x: 1,
                y: 2,
                ticks_x: 0,
                ticks_y: -3,
                mods: 1
            },
            Call::Key {
                view: id,
                press: true,
                keysym: 97,
                text: "a".into(),
                mods: 2
            },
            Call::Focus(id, true),
            Call::SetVisible(id, false),
            Call::Refresh(id),
        ]
    );
}

#[test]
fn a_view_the_engine_loses_is_lost_until_recreated_and_the_page_comes_back() {
    let (world, mut view) = view(7);
    view.create(4, 2).unwrap();
    let first = view.view_id().unwrap();
    view.set_html("<p>page</p>", "about:blank").unwrap();
    world.borrow().paint(first, 4, 2, BLUE);
    view.poll();

    world.borrow().queue.view_lost(first, 2);
    let events = view.poll();
    assert_eq!(events, vec![ViewEvent::Lost(Lost::Engine(2))]);
    assert_eq!(Lost::Engine(2).name(), "crashed");
    assert!(!view.is_live());
    // Nothing more is asked of a lost view: the client says so itself.
    assert_eq!(
        view.set_html("<p>again</p>", ""),
        Err(Error::Lost(Lost::Engine(2)))
    );
    assert!(view.poll().is_empty());
    // The picture is still on show.
    assert_eq!(view.picture().dominant(), Colour::Blue);

    view.recreate().unwrap();
    let second = view.view_id().unwrap();
    assert_ne!(first, second);
    assert!(view.is_live());
    assert_eq!(view.lost(), None);
    assert!(world.borrow().engine.calls().iter().any(|call| matches!(
        call,
        Call::LoadHtml { view, html, .. } if *view == second && html == b"<p>page</p>"
    )));
    world.borrow().paint(second, 4, 2, RED);
    assert_eq!(
        frames(&view.poll()),
        vec![(1, 4, 2)],
        "a new view counts from 1"
    );
    assert_eq!(view.picture().dominant(), Colour::Red);
    // The lost view was destroyed at webd, not left in its table.
    assert_eq!(world.borrow().service.view_count(), 1);
}

#[test]
fn destroy_frees_everything_on_both_sides() {
    let (world, mut view) = view(7);
    view.create(4, 2).unwrap();
    view.set_html("<p>x</p>", "").unwrap();
    let id = view.view_id().unwrap();
    world.borrow().paint(id, 4, 2, BLUE);
    view.poll();
    view.destroy();
    assert_eq!(view.view_id(), None);
    assert!(view.picture().is_empty());
    {
        let world = world.borrow();
        assert_eq!(world.service.view_count(), 0);
        assert_eq!(world.fabric.open_handles(), 0, "no handle left in webd");
        assert_eq!(world.fabric.double_closes(), 0);
    }
    // Calls on a destroyed view say so; dropping a view with none is quiet.
    assert_eq!(view.set_html("<p>x</p>", ""), Err(Error::NoView));
}

#[test]
fn dropping_a_view_destroys_it() {
    let (world, view) = view(7);
    {
        let mut view = view;
        view.create(4, 2).unwrap();
        assert_eq!(world.borrow().service.view_count(), 1);
    }
    let world = world.borrow();
    assert_eq!(world.service.view_count(), 0);
    assert_eq!(world.fabric.open_handles(), 0);
}

#[test]
fn two_slots_are_enough_and_the_hidden_state_is_told_to_a_new_view() {
    let world = support::world();
    let mut view =
        LightWebView::new(TestTransport::new(&world, 7), LightConfig::default()).with_slots(2);
    view.set_visible(false);
    view.focus(true);
    view.create(4, 2).unwrap();
    let id = view.view_id().unwrap();
    {
        let calls = world.borrow().engine.calls();
        assert!(calls.contains(&Call::SetVisible(id, false)));
        assert!(calls.contains(&Call::Focus(id, true)));
    }
    // Two slots: the third frame has nowhere to go until one is released.
    for shade in 1..=3u8 {
        let mut world = world.borrow_mut();
        world.paint(id, 4, 2, [shade, 0, 0, 255]);
        world.pump();
    }
    assert_eq!(frames(&view.poll()), vec![(2, 4, 2)]);
}
