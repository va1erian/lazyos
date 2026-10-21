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
const WHITE: [u8; 4] = [255, 255, 255, 255];

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
fn a_refused_call_leaves_the_view_as_it_was() {
    let (world, mut view) = view(7);
    view.create(4, 2).unwrap();
    assert_eq!(view.resize(0, 5), Err(Error::Refused(22)));
    assert_eq!(view.size(), (4, 2));
    assert_eq!(
        view.set_html("<p>x</p>", "http://example.com/"),
        Err(Error::Refused(22)),
        "a network base is refused"
    );
    assert!(view.is_live());
    let id = view.view_id().unwrap();
    world.borrow().paint(id, 4, 2, BLUE);
    assert_eq!(frames(&view.poll()).len(), 1, "it still paints");
}

#[test]
fn the_ninth_view_of_an_app_is_refused_and_the_others_keep_working() {
    let world = support::world();
    let mut views: Vec<_> = (0..8)
        .map(|_| {
            let mut view = LightWebView::new(TestTransport::new(&world, 7), LightConfig::default());
            view.create(2, 2).unwrap();
            view
        })
        .collect();
    let mut ninth = LightWebView::new(TestTransport::new(&world, 7), LightConfig::default());
    assert_eq!(ninth.create(2, 2), Err(Error::Refused(28)));
    assert!(ninth.view_id().is_none());
    let id = views[0].view_id().unwrap();
    world.borrow().paint(id, 2, 2, BLUE);
    assert_eq!(frames(&views[0].poll()).len(), 1);
    views.clear();
    assert_eq!(world.borrow().service.view_count(), 0);
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
fn a_service_that_is_not_running_is_not_running_and_one_that_returns_is_used() {
    let (world, mut view) = view(7);
    world.borrow_mut().down = true;
    assert_eq!(view.create(4, 2), Err(Error::NotRunning));
    assert_eq!(view.view_id(), None);
    assert_eq!(view.set_html("<p>x</p>", ""), Err(Error::NoView));
    view.mouse(MouseKind::Move, 0, 1, 1, 0);
    assert!(view.poll().is_empty());

    world.borrow_mut().down = false;
    view.create(4, 2).unwrap();
    assert!(view.is_live());
}

#[test]
fn a_service_that_goes_away_mid_life_loses_the_view_without_a_panic() {
    let (world, mut view) = view(7);
    view.create(4, 2).unwrap();
    view.set_html("<p>page</p>", "").unwrap();

    {
        let mut world = world.borrow_mut();
        world.down = true;
        world.fail_recv = Some(-32);
    }
    assert_eq!(view.poll(), vec![ViewEvent::Lost(Lost::ServiceGone)]);
    assert_eq!(view.lost(), Some(Lost::ServiceGone));
    assert_eq!(
        view.set_html("<p>x</p>", ""),
        Err(Error::Lost(Lost::ServiceGone))
    );
    assert_eq!(view.resize(8, 8), Err(Error::Lost(Lost::ServiceGone)));
    view.key(true, 97, "a", 0);
    assert!(view.poll().is_empty(), "the loss is reported once");
    // Recreating while it is still down fails cleanly and leaves a lost, empty view.
    assert_eq!(view.recreate(), Err(Error::NotRunning));

    // webd comes back: the same call makes a new view and loads the page again.
    {
        let mut world = world.borrow_mut();
        world.down = false;
        world.fail_recv = None;
    }
    view.recreate().unwrap();
    let id = view.view_id().unwrap();
    assert!(world.borrow().engine.calls().iter().any(|call| matches!(
        call,
        Call::LoadHtml { view, html, .. } if *view == id && html == b"<p>page</p>"
    )));
}

#[test]
fn a_call_that_finds_the_view_gone_loses_it() {
    let (world, mut view) = view(7);
    view.create(4, 2).unwrap();
    // webd restarted: it has no such view (ENOENT).
    let id = view.view_id().unwrap();
    world.borrow_mut().service.shutdown();
    assert_eq!(world.borrow().service.view_count(), 0);
    assert_eq!(
        view.set_html("<p>x</p>", ""),
        Err(Error::Lost(Lost::ServiceGone))
    );
    assert_eq!(view.lost(), Some(Lost::ServiceGone));
    assert!(id > 0);
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

#[test]
fn garbage_events_are_ignored_and_a_stranger_views_events_are_not_ours() {
    let (world, mut view) = view(7);
    view.create(4, 2).unwrap();
    let id = view.view_id().unwrap();
    let channel = view.events_handle().unwrap();
    // Not events, truncated events, events of other views, out-of-range slots, absurd sizes.
    let other = wire::encode_frame_args(&wire::FrameArgs {
        view: id + 1000,
        slot: 0,
        seq: 9,
        width: 4,
        height: 2,
    })
    .unwrap();
    let bad_slot = wire::encode_frame_args(&wire::FrameArgs {
        view: id,
        slot: 99,
        seq: 9,
        width: 4,
        height: 2,
    })
    .unwrap();
    let huge = wire::encode_frame_args(&wire::FrameArgs {
        view: id,
        slot: 0,
        seq: 9,
        width: u32::MAX,
        height: u32::MAX,
    })
    .unwrap();
    {
        let mut world = world.borrow_mut();
        world.inject(channel, 1, vec![1, 2, 3]);
        world.inject(channel, wire::METHOD_FRAME, vec![0xff; 5]);
        world.inject(channel, wire::METHOD_FRAME, other);
        world.inject(channel, wire::METHOD_FRAME, bad_slot);
        world.inject(channel, wire::METHOD_FRAME, huge);
        world.inject(channel, wire::METHOD_PING, Vec::new());
    }
    assert!(view.poll().is_empty());
    assert!(view.picture().is_empty());
    assert!(view.is_live());
}

#[test]
fn random_bytes_as_events_never_panic() {
    let (world, mut view) = view(7);
    view.create(4, 2).unwrap();
    let channel = view.events_handle().unwrap();
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..2000 {
        let method = [100, 101, 102, 103, 104, 105, 106, 1, 0][(next() % 9) as usize];
        let len = (next() % 64) as usize;
        let body: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        world.borrow_mut().inject(channel, method, body);
    }
    let _ = view.poll();
    while !view.poll().is_empty() {}
    // Whatever it made of them, a clean page still paints.
    let id = view.view_id().unwrap();
    if view.is_live() {
        world.borrow().paint(id, 4, 2, WHITE);
        let _ = view.poll();
    }
}
