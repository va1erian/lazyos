//! Tests of the queue between WebKit's thread and the service's.

use super::*;
use crate::limits::QUEUE_KEPT;

#[test]
fn a_newer_frame_replaces_the_one_not_yet_seen() {
    let queue = EventQueue::new();
    queue.frame(1, &[1; 16], 2, 2);
    queue.frame(1, &[2; 16], 2, 2);
    queue.frame(2, &[3; 16], 2, 2);
    let drained = queue.drain();
    assert_eq!(drained.frames.len(), 2);
    assert_eq!(drained.frames[&1].rgba, vec![2; 16]);
    assert!(queue.drain().is_empty());
}

#[test]
fn the_latest_state_and_scroll_win_and_a_lost_view_forgets_them() {
    let queue = EventQueue::new();
    queue.state(1, "a", true, 0, "");
    queue.state(1, "b", false, 1, "");
    queue.scroll(1, 0, 10, false);
    queue.scroll(1, 0, 20, true);
    queue.state(2, "x", false, 1, "");
    queue.frame(2, &[0; 4], 1, 1);
    queue.view_lost(2, 2);
    let drained = queue.drain();
    assert_eq!(drained.states[&1].title, "b");
    assert_eq!(drained.scrolls[&1], (0, 20, true));
    assert!(!drained.states.contains_key(&2) && drained.frames.is_empty());
    assert_eq!(drained.once, vec![Once::Lost { view: 2, reason: 2 }]);
}

#[test]
fn ordered_events_keep_their_order_and_the_queue_is_bounded() {
    let queue = EventQueue::new();
    for n in 0..QUEUE_KEPT + 10 {
        queue.link_clicked(1, &format!("https://x/{n}"));
    }
    assert_eq!(queue.dropped(), 10);
    let once = queue.drain().once;
    assert_eq!(once.len(), QUEUE_KEPT);
    assert_eq!(
        once[0],
        Once::LinkClicked {
            view: 1,
            url: "https://x/10".into()
        }
    );
}

#[test]
fn a_page_cannot_queue_more_than_the_text_limit_per_event() {
    let queue = EventQueue::new();
    // A 50 MB URL, a 50 MB copy, a title and an error of the same size, a marker line, all of
    // them cut where they are stored (multi-byte characters: the cut must stay on a boundary).
    let huge = "é".repeat(25 << 20);
    queue.link_clicked(1, &huge);
    queue.clipboard_write(1, &huge);
    queue.state(1, &huge, false, 1, &huge);
    queue.say(&huge);
    let drained = queue.drain();
    let limit = EVENT_TEXT_BYTES;
    assert_eq!(drained.states[&1].title.len(), limit);
    assert_eq!(drained.states[&1].error.len(), limit);
    let sizes: Vec<usize> = drained
        .once
        .iter()
        .map(|event| match event {
            Once::LinkClicked { url, .. } => url.len(),
            Once::ClipboardWrite { text, .. } => text.len(),
            Once::Say(line) => line.len(),
            _ => 0,
        })
        .collect();
    assert_eq!(sizes, vec![limit, limit, limit]);
    // A short text is stored whole.
    queue.link_clicked(1, "https://x/");
    assert_eq!(
        queue.drain().once,
        vec![Once::LinkClicked {
            view: 1,
            url: "https://x/".into()
        }]
    );
}

#[test]
fn the_wake_rings_for_every_event_and_the_memory_source_answers() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let rings = Arc::new(AtomicUsize::new(0));
    let counter = rings.clone();
    let queue = EventQueue::new()
        .with_wake(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .with_memory(|| Some((5, 9)));
    queue.say("hello");
    queue.scroll(1, 0, 0, false);
    assert_eq!(rings.load(Ordering::SeqCst), 2);
    assert_eq!(queue.user_memory(), Some((5, 9)));
    assert_eq!(EventQueue::new().user_memory(), None);
}

#[test]
fn a_frame_the_service_gave_back_is_the_next_frames_buffer() {
    let queue = EventQueue::new();
    queue.frame(1, &[1; 64], 4, 4);
    let first = queue.drain().frames.remove(&1).unwrap();
    let at = first.rgba.as_ptr();
    queue.recycle(first.rgba);
    queue.frame(1, &[2; 64], 4, 4);
    let second = queue.drain().frames.remove(&1).unwrap();
    assert_eq!(second.rgba, vec![2; 64]);
    assert_eq!(second.rgba.as_ptr(), at, "no new allocation");
}

#[test]
fn a_view_with_deltas_reuses_frames_bands_and_patches() {
    let queue = EventQueue::new();
    queue.track_pictures(1);
    queue.frame(1, &[1; 64], 4, 4);
    queue.band(1, 0, 4, 4, 2, &[7; 32]);
    let mut drained = queue.drain();
    let whole = drained.pictures.remove(&1).unwrap().whole.unwrap().0;
    let band = drained.bands.remove(&1).unwrap().remove(0);
    let (whole_at, band_at) = (whole.rgba.as_ptr(), band.rgba.as_ptr());
    queue.recycle(whole.rgba);
    queue.recycle(band.rgba);
    // A one-pixel delta takes the smaller buffer (the band's), the next frame the larger.
    assert!(!queue.frame_delta(1, &[3; 64], 4, 4, None, &[[0, 0, 1, 1]]));
    let delta = queue.drain().pictures.remove(&1).unwrap().deltas.remove(0);
    assert_eq!(delta.patches[0].rgba.as_ptr(), band_at);
    queue.frame(1, &[4; 64], 4, 4);
    let whole = queue.drain().pictures.remove(&1).unwrap().whole.unwrap().0;
    assert_eq!((whole.rgba.as_ptr(), whole.rgba.len()), (whole_at, 64));
    queue.trim_spares();
    assert_eq!(queue.spare_bytes(), 0);
}
