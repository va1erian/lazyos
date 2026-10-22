//! The pictures of a full view: whole frames, the deltas of a view made with `deltas` (each into a
//! slot of its own, applied on top of the picture the client has), bands in the extra slots, and what
//! happens when the client cannot take one.

#![cfg(not(feature = "host"))]

mod support;

use messenger_generated::os_lazy_webd_view_v1 as wire;
use support::full::{data_only, FEv, Fx, Via};
use support::origin;
use xui_webd::engine::EngineEvents;
use xui_webd::fabric::errno::*;
use xui_webd::recording::Call as EngineCall;
use xui_webd::Note;

const BAND_BYTES: usize = 4 * 1024 * 4;

fn deltas() -> wire::ViewConfig {
    wire::ViewConfig {
        deltas: true,
        ..data_only()
    }
}

/// A `w` x `h` RGBA picture whose pixel `(x, y)` is `fill(x, y)` in all four bytes.
fn frame(w: i32, h: i32, fill: impl Fn(i32, i32) -> u8) -> Vec<u8> {
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            out.extend_from_slice(&[fill(x, y); 4]);
        }
    }
    out
}

fn whole_frame(view: u64, slot: u32, seq: u64) -> wire::FrameArgs {
    wire::FrameArgs {
        view,
        slot,
        seq,
        width: 4,
        height: 4,
        whole: true,
        ..wire::FrameArgs::default()
    }
}

#[test]
fn a_whole_frame_then_a_delta_arrive_in_two_slots_with_the_shift_the_rects_and_the_offset() {
    // Why: a delta only means something on top of the picture the client has; the client moves
    // its picture by `shift`, then copies `rects` from the delta's slot, where they sit at their
    // place in a whole frame's layout.
    let mut fx = Fx::new("frames-delta");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::View, deltas(), 4, 4);
    let slots = fx.attach_frames(app, view, 4, 4);
    fx.engine.take();

    let old = frame(4, 4, |x, y| (y * 4 + x) as u8);
    fx.queue.frame(view, &old, 4, 4);
    fx.pump(0);
    assert_eq!(fx.news(events), vec![FEv::Frame(whole_frame(view, 0, 1))]);
    assert_eq!(*slots[0].borrow(), old);

    // The page scrolled down one row: the rows move up and the last row is repainted.
    let new = frame(
        4,
        4,
        |x, y| if y < 3 { ((y + 1) * 4 + x) as u8 } else { 200 },
    );
    fx.queue.scroll(view, 0, 1, true);
    let shift = Some([0, 0, 4, 4, 0, -1]);
    assert!(!fx
        .queue
        .frame_delta(view, &new, 4, 4, shift, &[[0, 3, 4, 1]]));
    fx.pump(1);
    let delta = wire::FrameArgs {
        view,
        slot: 1,
        seq: 2,
        width: 4,
        height: 4,
        whole: false,
        shift: Some(wire::Shift {
            x: 0,
            y: 0,
            width: 4,
            height: 4,
            dx: 0,
            dy: -1,
        }),
        rects: vec![wire::Rect {
            x: 0,
            y: 3,
            width: 4,
            height: 1,
        }],
        offset_x: 0,
        offset_y: 1,
        at_bottom: true,
    };
    assert_eq!(fx.news(events), vec![FEv::Frame(delta)]);
    // Only the rectangle was written, at its place (the fourth row); the rest of the slot is as
    // it was, and the slot the client still holds is untouched.
    let slot = slots[1].borrow();
    assert!(slot[..48].iter().all(|b| *b == 0));
    assert_eq!(slot[48..], new[48..]);
    assert_eq!(*slots[0].borrow(), old);
}

#[test]
fn a_delta_that_comes_while_the_whole_frame_waits_is_folded_into_it() {
    // Why: the service sends one picture, not a frame and a chain of deltas, when it was too slow
    // to see the frame the deltas apply to.
    let mut fx = Fx::new("frames-absorb");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::View, deltas(), 4, 4);
    let slots = fx.attach_frames(app, view, 4, 4);
    let old = frame(4, 4, |x, y| (y * 4 + x) as u8);
    let new = frame(
        4,
        4,
        |x, y| if y < 3 { ((y + 1) * 4 + x) as u8 } else { 200 },
    );
    fx.queue.scroll(view, 0, 0, false);
    fx.queue.frame(view, &old, 4, 4);
    fx.queue.scroll(view, 0, 1, true);
    let shift = Some([0, 0, 4, 4, 0, -1]);
    assert!(!fx
        .queue
        .frame_delta(view, &new, 4, 4, shift, &[[0, 3, 4, 1]]));
    fx.pump(0);
    let one = wire::FrameArgs {
        offset_y: 1,
        at_bottom: true,
        ..whole_frame(view, 0, 1)
    };
    assert_eq!(fx.news(events), vec![FEv::Frame(one)]);
    assert_eq!(
        *slots[0].borrow(),
        new,
        "the picture with the delta applied"
    );
    assert!(
        slots[1].borrow().iter().all(|b| *b == 0),
        "no second slot was used"
    );
}

#[test]
fn a_delta_without_a_free_slot_is_dropped_the_chain_ends_and_a_release_asks_for_a_whole_frame() {
    // Why: the client cannot apply a delta whose predecessor it never saw, so the first picture
    // it cannot take ends the chain; the engine is told (the queue refuses deltas) and paints a
    // whole frame once a slot is free again.
    let mut fx = Fx::new("frames-drop");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::View, deltas(), 4, 4);
    for slot in 0..2 {
        fx.attach_slot(app, view, slot, 64).0.unwrap();
    }
    let picture = frame(4, 4, |x, y| (y * 4 + x) as u8);
    fx.queue.frame(view, &picture, 4, 4);
    fx.pump(0);
    assert_eq!(fx.news(events).len(), 1, "the whole frame took slot 0");
    fx.engine.take();

    assert!(!fx
        .queue
        .frame_delta(view, &picture, 4, 4, None, &[[0, 3, 4, 1]]));
    assert!(!fx
        .queue
        .frame_delta(view, &picture, 4, 4, None, &[[0, 2, 4, 1]]));
    fx.pump(1);
    let sent = fx.news(events);
    assert_eq!(sent.len(), 1, "the first delta took slot 1");
    assert!(matches!(&sent[0], FEv::Frame(f) if f.slot == 1 && !f.whole && f.seq == 2));
    assert!(fx.service.take_notes().contains(&Note::FrameDropped {
        view,
        why: "no free slot"
    }));
    // The second delta was lost, so none can apply any more: the queue refuses them.
    assert!(fx
        .queue
        .frame_delta(view, &picture, 4, 4, None, &[[0, 1, 4, 1]]));
    assert!(
        fx.engine.take().is_empty(),
        "no slot is free to paint into yet"
    );

    // A slot comes back: the engine is asked for a whole frame, once.
    fx.release_slot(app, view, 0).unwrap();
    fx.release_slot(app, view, 1).unwrap();
    assert_eq!(fx.engine.take(), vec![EngineCall::Refresh(view)]);
    fx.queue.frame(view, &picture, 4, 4);
    fx.pump(2);
    let sent = fx.news(events);
    assert!(matches!(&sent[..], [FEv::Frame(f)] if f.slot == 0 && f.whole && f.seq == 3));
    // And deltas have a base again.
    assert!(!fx
        .queue
        .frame_delta(view, &picture, 4, 4, None, &[[0, 3, 4, 1]]));
}

#[test]
fn bands_use_the_two_band_slots_which_need_a_whole_rows_worth_of_bytes() {
    // Why: a band is rows of the page outside the view, painted ahead of a scroll; its slot holds
    // `width * 1024 * 4` bytes, and only a view with deltas has band slots.
    let mut fx = Fx::new("frames-bands");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::View, deltas(), 4, 4);
    assert_eq!(fx.attach_slot(app, view, 3, 64).0, Err(EINVAL));
    assert_eq!(fx.attach_slot(app, view, 3, BAND_BYTES - 1).0, Err(EINVAL));
    assert_eq!(fx.attach_slot(app, view, 5, BAND_BYTES).0, Err(EINVAL));
    let (result, band3) = fx.attach_slot(app, view, 3, BAND_BYTES);
    result.unwrap();
    let (result, band4) = fx.attach_slot(app, view, 4, BAND_BYTES);
    result.unwrap();

    let band = |shade: u8, doc_y: i32| {
        let rows = vec![shade; 4 * 2 * 4];
        (rows, doc_y)
    };
    for (shade, doc_y) in [(0xAA, 100), (0xBB, 102), (0xCC, 104)] {
        let (rows, doc_y) = band(shade, doc_y);
        fx.queue.band(view, 0, doc_y, 4, 2, &rows);
    }
    fx.pump(0);
    let expected = |slot, doc_y| {
        FEv::Band(wire::BandArgs {
            view,
            slot,
            doc_x: 0,
            doc_y,
            width: 4,
            height: 2,
        })
    };
    assert_eq!(fx.news(events), vec![expected(3, 100), expected(4, 102)]);
    assert!(band3.borrow()[..32].iter().all(|b| *b == 0xAA));
    assert!(band4.borrow()[..32].iter().all(|b| *b == 0xBB));
    assert!(fx
        .service
        .take_notes()
        .iter()
        .all(|n| !matches!(n, Note::FrameDropped { .. })));

    // A band as wide as the view is the only band: other widths, and bands taller than the slot, are
    // dropped, and a dropped band does not keep its slot.
    fx.release_slot(app, view, 3).unwrap();
    fx.queue.band(view, 0, 0, 5, 1, &[1; 20]);
    fx.queue.band(view, 0, 0, 4, 2000, &vec![2; 4 * 2000 * 4]);
    fx.queue.band(view, 0, 200, 4, 1, &[3; 16]);
    fx.pump(1);
    assert_eq!(
        fx.news(events),
        vec![FEv::Band(wire::BandArgs {
            view,
            slot: 3,
            doc_x: 0,
            doc_y: 200,
            width: 4,
            height: 1
        })]
    );
    assert_eq!(band3.borrow()[..16], [3; 16]);
    // Bands never use a frame slot, and whole frames never a band slot.
    let picture = frame(4, 4, |_, _| 9);
    fx.queue.frame(view, &picture, 4, 4);
    fx.pump(2);
    assert!(fx.news(events).is_empty(), "no frame slot is attached");
    assert_eq!(fx.fabric.double_closes(), 0);
}

#[test]
fn only_a_view_made_with_deltas_has_band_slots_and_only_it_takes_deltas() {
    // Why: slots 3 and 4 and the delta queue exist per view, made when the view is made; a light
    // view or a view without deltas must not get them by asking.
    let mut fx = Fx::new("frames-no-deltas");
    let app = origin(7);
    let (plain, events) = fx.view(app, Via::View, data_only(), 4, 4);
    let (lite, _) = fx.rig.create_plain(app, 4, 4);
    for slot in [3, 4] {
        assert_eq!(fx.attach_slot(app, plain, slot, BAND_BYTES).0, Err(EINVAL));
        assert_eq!(
            fx.rig.attach_bytes(app, lite, slot, BAND_BYTES).0,
            Err(EINVAL)
        );
    }
    fx.attach_frames(app, plain, 4, 4);
    let picture = frame(4, 4, |_, _| 1);
    assert!(
        fx.queue
            .frame_delta(plain, &picture, 4, 4, None, &[[0, 0, 1, 1]]),
        "refused"
    );
    fx.queue.band(plain, 0, 0, 4, 1, &picture[..16]);
    fx.queue.frame(plain, &picture, 4, 4);
    fx.queue.scroll(plain, 3, 4, true);
    fx.pump(0);
    let sent = fx.news(events);
    // A view without deltas has plain whole frames and plain scroll events.
    assert_eq!(sent.len(), 2);
    assert!(sent.contains(&FEv::Frame(whole_frame(plain, 0, 1))));
    assert!(sent.contains(&FEv::Scroll(wire::ScrollArgs {
        view: plain,
        x: 3,
        y: 4,
        at_bottom: true
    })));
    // Unknown views take no delta and no band.
    assert!(fx.queue.frame_delta(9999, &picture, 4, 4, None, &[]));
}

#[test]
fn frame_slots_must_fit_the_view_and_a_busy_one_cannot_be_replaced() {
    // Why: the service writes a whole picture into a slot it did not allocate.
    let mut fx = Fx::new("frames-slots");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::View, data_only(), 4, 4);
    assert_eq!(fx.attach_slot(app, view, 0, 63).0, Err(EINVAL));
    assert_eq!(fx.attach_slot(app, view, 3, 64).0, Err(EINVAL));
    assert!(fx.attach_slot(app, view, 0, 64).0.is_ok());
    assert!(
        fx.attach_slot(app, view, 0, 4096).0.is_ok(),
        "bigger is fine and replaces it"
    );
    fx.queue.frame(view, &frame(4, 4, |_, _| 5), 4, 4);
    fx.pump(0);
    assert_eq!(fx.news(events).len(), 1);
    let handles = fx.fabric.open_handles();
    assert_eq!(fx.attach_slot(app, view, 0, 64).0, Err(EBUSY));
    assert_eq!(
        fx.fabric.open_handles(),
        handles,
        "the refused buffer was closed"
    );
}

#[test]
fn a_resize_detaches_every_slot_and_a_picture_of_the_old_size_is_dropped() {
    // Why: every slot was sized for the old picture, and a delta of the old size cannot apply to
    // a picture of the new one.
    let mut fx = Fx::new("frames-resize");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::View, deltas(), 4, 4);
    fx.attach_frames(app, view, 4, 4);
    fx.attach_slot(app, view, 3, BAND_BYTES).0.unwrap();
    fx.engine.take();
    let old = frame(4, 4, |_, _| 1);
    fx.queue.frame(view, &old, 4, 4);
    fx.resize_view(app, view, 8, 8).unwrap();
    assert_eq!(fx.engine.take(), vec![EngineCall::Resize(view, 8, 8)]);
    fx.pump(0);
    assert!(fx.news(events).is_empty());
    assert!(fx.service.take_notes().contains(&Note::FrameDropped {
        view,
        why: "wrong size"
    }));
    assert!(
        fx.queue
            .frame_delta(view, &old, 4, 4, None, &[[0, 0, 1, 1]]),
        "no base"
    );
    fx.queue.band(view, 0, 0, 4, 1, &old[..16]);
    fx.pump(1);
    assert!(
        fx.news(events).is_empty(),
        "the band slot went with the rest"
    );

    // The client attaches slots of the new size; attaching asks for a fresh picture.
    fx.engine.take();
    let shared = fx.attach_frames(app, view, 8, 8);
    assert!(fx.engine.take().contains(&EngineCall::Refresh(view)));
    fx.queue.frame(view, &frame(8, 8, |_, _| 7), 8, 8);
    fx.pump(2);
    let sent = fx.news(events);
    assert!(matches!(&sent[..], [FEv::Frame(f)] if f.whole && (f.width, f.height) == (8, 8)));
    assert!(shared[0].borrow().iter().all(|b| *b == 7));
}

#[test]
fn a_client_that_stopped_reading_loses_the_picture_and_the_next_one_waits_out_the_back_off() {
    // Why: a delta the client never got ends the chain just like a missing slot; the slot goes
    // back to free and the whole frame is asked for again once the wait is over.
    let mut fx = Fx::new("frames-full-queue");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::View, deltas(), 4, 4);
    let slots = fx.attach_frames(app, view, 4, 4);
    let picture = frame(4, 4, |_, _| 9);
    fx.fabric.set_full(events, true);
    fx.engine.take();
    fx.queue.frame(view, &picture, 4, 4);
    fx.pump(0);
    assert_eq!(*slots[0].borrow(), picture, "the copy was made and lost");
    assert!(
        fx.queue
            .frame_delta(view, &picture, 4, 4, None, &[[0, 0, 1, 1]]),
        "the chain ended"
    );
    fx.fabric.set_full(events, false);
    fx.pump(100);
    assert!(
        fx.engine.take().is_empty(),
        "no picture is asked for while backing off"
    );
    fx.pump(300);
    assert_eq!(fx.engine.take(), vec![EngineCall::Refresh(view)]);
    fx.queue.frame(view, &picture, 4, 4);
    fx.pump(300);
    // The lost frame's number and slot are reused: the client never heard of them.
    assert_eq!(fx.news(events), vec![FEv::Frame(whole_frame(view, 0, 1))]);
}
