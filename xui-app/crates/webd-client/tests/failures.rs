//! What the client does when the service refuses, vanishes, is not there, or sends nonsense: the view
//! is lost or left as it was, never a panic or a hang.

mod support;

use messenger_generated::os_lazy_webd_light_v1 as wire;
use support::{view, TestTransport};
use xui_webd::recording::Call;
use xui_webd_client::{Error, LightConfig, LightWebView, Lost, MouseKind, ViewEvent};

const BLUE: [u8; 4] = [0, 0, 255, 255];
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
