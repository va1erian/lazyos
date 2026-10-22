//! A full view's pictures against the service half: whole frames, deltas (a shift, then rectangles
//! out of the slot) and bands, played by calling the queue the way the engine's callbacks do. The
//! picture the client keeps must be exactly the page, frame after frame.

mod support;

use messenger_generated::os_lazy_webd_view_v1 as wire;
use xui_webd::engine::EngineEvents;
use xui_webd::events::pictures::{apply_patch, apply_shift, cut, Patch};
use xui_webd::events::FrameNow;
use xui_webd_client::full::{FullEvent, Scope};

use support::full::{net_config, web, Full};
use support::web::long_frame;

const W: i32 = 64;
const H: i32 = 48;

/// A view with deltas on a fresh world, made; its id.
fn made(deltas: bool) -> (support::full::Web, Full, u64) {
    let web = web();
    let config = wire::ViewConfig {
        deltas,
        ..net_config()
    };
    let mut view = web.view(7, Scope::Net, config);
    view.create(W as u32, H as u32).expect("create");
    let id = view.view_id().unwrap();
    (web, view, id)
}

/// Poll until nothing more arrives; every event.
fn drain(view: &mut Full) -> Vec<FullEvent> {
    let mut all = Vec::new();
    for _ in 0..4 {
        all.extend(view.poll());
    }
    all
}

fn frames(events: &[FullEvent]) -> Vec<xui_webd_client::full::FrameInfo> {
    events
        .iter()
        .filter_map(|event| match event {
            FullEvent::Frame(info) => Some(info.clone()),
            _ => None,
        })
        .collect()
}

/// The page scrolls from `from` to `to`: a shift of the whole view and the rows it uncovers.
fn scroll(web: &support::full::Web, id: u64, from: i32, to: i32) {
    let queue = web.world.borrow().queue.clone();
    let dy = from - to;
    let rect = if dy < 0 {
        [0, H + dy, W, -dy]
    } else {
        [0, 0, W, dy]
    };
    queue.scroll(id, 0, to, false);
    let refused = queue.frame_delta(
        id,
        &long_frame(W, H, to),
        W,
        H,
        Some([0, 0, W, H, 0, dy]),
        &[rect],
    );
    assert!(!refused, "the queue took the delta");
}

#[test]
fn a_whole_frame_then_deltas_rebuild_the_page_exactly() {
    let (web, mut view, id) = made(true);
    let queue = web.world.borrow().queue.clone();
    queue.frame(id, &long_frame(W, H, 0), W, H);
    let first = frames(&drain(&mut view));
    assert_eq!(first.len(), 1);
    assert!(first[0].whole && first[0].applied);
    assert_eq!(view.picture().rgba(), &long_frame(W, H, 0)[..]);
    let mut y = 0;
    // Down, up, nearly a whole view down (one row stays), back up to the top.
    for to in [10, 30, 23, 23 + H - 1, 40, 2, 0, 17] {
        scroll(&web, id, y, to);
        let got = frames(&drain(&mut view));
        assert_eq!(got.len(), 1, "one delta for the scroll to {to}");
        let frame = &got[0];
        assert!(!frame.whole && frame.applied, "{frame:?}");
        assert_eq!(frame.shift.as_ref().map(|s| s.dy), Some(y - to));
        assert_eq!(frame.rects.len(), 1);
        assert_eq!(frame.offset, (0, to, false));
        assert_eq!(
            view.picture().rgba(),
            &long_frame(W, H, to)[..],
            "the picture at y={to}"
        );
        y = to;
    }
    let counters = view.counters();
    assert_eq!(
        (counters.whole, counters.deltas, counters.ignored),
        (1, 8, 0)
    );
}

#[test]
fn a_partial_shift_sideways_matches_the_service_own_reconstruction() {
    let (web, mut view, id) = made(true);
    let queue = web.world.borrow().queue.clone();
    // A picture where every pixel differs.
    let noise = |seed: u32| -> Vec<u8> {
        (0..(W * H * 4) as u32)
            .map(|i| (i.wrapping_mul(2654435761).wrapping_add(seed) >> 13) as u8)
            .collect()
    };
    let old = noise(1);
    queue.frame(id, &old, W, H);
    drain(&mut view);
    let new = noise(2);
    let shift = [5, 3, 40, 30, 2, -4];
    let rects = [[0, 0, 7, 9], [40, 20, 24, 28], [6, 33, 30, 1]];
    assert!(!queue.frame_delta(id, &new, W, H, Some(shift), &rects));
    let got = frames(&drain(&mut view));
    assert!(got.len() == 1 && got[0].applied && !got[0].whole);
    // What the service's own rules say the client must now show.
    let mut want = FrameNow {
        width: W,
        height: H,
        rgba: old,
    };
    apply_shift(&mut want, shift);
    for rect in rects {
        let patch = Patch {
            rect,
            rgba: cut(&new, W, H, rect).unwrap(),
        };
        apply_patch(&mut want, &patch);
    }
    assert_eq!(view.picture().rgba(), &want.rgba[..]);
}

#[test]
fn deltas_of_one_turn_apply_in_order() {
    let (web, mut view, id) = made(true);
    let queue = web.world.borrow().queue.clone();
    queue.frame(id, &long_frame(W, H, 0), W, H);
    drain(&mut view);
    // Three scrolls before the client looks: three deltas, one per free slot, in order.
    scroll(&web, id, 0, 8);
    scroll(&web, id, 8, 20);
    scroll(&web, id, 20, 12);
    let got = frames(&drain(&mut view));
    assert_eq!(got.len(), 3);
    assert!(got.iter().all(|f| f.applied && !f.whole));
    assert!(got.windows(2).all(|w| w[1].seq == w[0].seq + 1));
    assert_eq!(view.picture().rgba(), &long_frame(W, H, 12)[..]);
    // Every slot came back: the next round goes through as well.
    for round in 0..20 {
        scroll(&web, id, 12 + round, 13 + round);
        assert!(frames(&drain(&mut view)).iter().all(|f| f.applied));
    }
    assert_eq!(view.picture().rgba(), &long_frame(W, H, 32)[..]);
}

#[test]
fn a_delta_with_nothing_under_it_is_refused_and_a_whole_frame_asked_for() {
    let (web, mut view, id) = made(true);
    let queue = web.world.borrow().queue.clone();
    queue.frame(id, &long_frame(W, H, 0), W, H);
    drain(&mut view);
    // A delta that skips a frame (as if one were lost on the way): the client cannot apply it.
    let channel = view.events_handle().unwrap();
    let args = wire::FrameArgs {
        view: id,
        slot: 1,
        seq: 99,
        width: W as u32,
        height: H as u32,
        whole: false,
        rects: vec![wire::Rect {
            x: 0,
            y: 0,
            width: 4,
            height: 4,
        }],
        ..wire::FrameArgs::default()
    };
    let body = wire::encode_frame_args(&args).unwrap();
    web.world
        .borrow_mut()
        .inject(channel, wire::METHOD_FRAME, body);
    let got = frames(&drain(&mut view));
    assert_eq!(got.len(), 1);
    assert!(!got[0].applied);
    assert_eq!(view.picture().rgba(), &long_frame(W, H, 0)[..], "untouched");
    let sent = &web.world.borrow().sent;
    assert!(sent.iter().any(|(m, _)| *m == wire::METHOD_REFRESH));
    assert!(sent.iter().any(|(m, _)| *m == wire::METHOD_RELEASESLOT));
    assert_eq!(view.counters().refreshes, 1);
}

#[test]
fn bands_carry_rows_below_the_view_and_their_slots_come_back() {
    let (web, mut view, id) = made(true);
    let queue = web.world.borrow().queue.clone();
    queue.frame(id, &long_frame(W, H, 0), W, H);
    drain(&mut view);
    for round in 0..3 {
        let doc_y = H + 20 * round;
        queue.band(id, 0, doc_y, W, 20, &long_frame(W, 20, doc_y));
        queue.band(id, 0, doc_y + 20, W, 20, &long_frame(W, 20, doc_y + 20));
        let bands: Vec<_> = drain(&mut view)
            .into_iter()
            .filter_map(|event| match event {
                FullEvent::Band(band) => Some(band),
                _ => None,
            })
            .collect();
        assert_eq!(bands.len(), 2, "round {round}: both band slots were free");
        assert_eq!(bands[0].doc_y, doc_y);
        assert_eq!(bands[0].rgba, long_frame(W, 20, doc_y));
        assert_eq!(bands[1].rgba, long_frame(W, 20, doc_y + 20));
    }
    assert_eq!(view.counters().bands, 6);
}

#[test]
fn a_view_without_deltas_has_no_band_slots_and_gets_whole_frames() {
    let (web, mut view, id) = made(false);
    let queue = web.world.borrow().queue.clone();
    queue.frame(id, &long_frame(W, H, 3), W, H);
    queue.band(id, 0, H, W, 20, &long_frame(W, 20, H));
    let events = drain(&mut view);
    let got = frames(&events);
    assert!(got.len() == 1 && got[0].whole && got[0].applied);
    assert!(!events.iter().any(|e| matches!(e, FullEvent::Band(_))));
    assert_eq!(view.picture().rgba(), &long_frame(W, H, 3)[..]);
}

#[test]
fn a_resize_waits_for_a_whole_frame_of_the_new_size() {
    let (web, mut view, id) = made(true);
    let queue = web.world.borrow().queue.clone();
    queue.frame(id, &long_frame(W, H, 0), W, H);
    drain(&mut view);
    view.resize(32, 24).expect("resize");
    // A delta of the old size is refused by the queue; the whole frame of the new size shows.
    queue.frame(id, &long_frame(32, 24, 9), 32, 24);
    let got = frames(&drain(&mut view));
    assert!(got.len() == 1 && got[0].whole && got[0].applied);
    assert_eq!(view.picture().width(), 32);
    assert_eq!(view.picture().rgba(), &long_frame(32, 24, 9)[..]);
}
