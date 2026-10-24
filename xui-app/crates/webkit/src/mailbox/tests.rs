use super::*;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

fn counted(mailbox: &Mailbox) -> Arc<AtomicUsize> {
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    mailbox.set_notify(move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
    });
    count
}

#[test]
fn a_burst_of_frames_wakes_the_window_once_and_keeps_the_newest() {
    let mailbox = Mailbox::default();
    let wakes = counted(&mailbox);
    mailbox.post_frame(2, 1, &[1; 8]);
    mailbox.post_frame(2, 1, &[2; 8]);
    mailbox.post_frame(2, 1, &[3; 8]);
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
    let frame = mailbox.take_frame(Vec::new()).unwrap();
    assert_eq!((frame.width, frame.height), (2, 1));
    assert_eq!(frame.rgba, vec![3; 8]);
    assert!(mailbox.take_frame(Vec::new()).is_none());
    // Collected: the next frame wakes it again.
    mailbox.post_frame(2, 1, &[4; 8]);
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
}

#[test]
fn state_is_replaced_not_queued() {
    let mailbox = Mailbox::default();
    let wakes = counted(&mailbox);
    for title in ["a", "b", "c"] {
        mailbox.post_state(PageState {
            title: title.into(),
            ..PageState::default()
        });
    }
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
    assert_eq!(mailbox.take_state().unwrap().title, "c");
    assert!(mailbox.take_state().is_none());
}

#[test]
fn what_arrived_before_the_window_wakes_it_when_it_attaches() {
    let mailbox = Mailbox::default();
    mailbox.post_frame(1, 1, &[9; 4]);
    mailbox.post_state(PageState::default());
    let wakes = counted(&mailbox);
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
}

#[test]
fn events_wake_the_window_once_per_burst_and_wait_for_a_window_that_attaches_late() {
    let mailbox = Mailbox::default();
    mailbox.post_event(Event::Hover("http://a/".into()));
    mailbox.post_event(Event::Popup { url: "b".into() });
    let wakes = counted(&mailbox);
    assert_eq!(
        wakes.load(Ordering::SeqCst),
        1,
        "what arrived first wakes it on attach"
    );
    mailbox.post_event(Event::Popup { url: "c".into() });
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
    assert_eq!(mailbox.take_events().len(), 3);
    mailbox.post_event(Event::Hover(String::new()));
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
}

#[test]
fn the_spare_buffer_comes_back_as_the_next_frames_storage() {
    let mailbox = Mailbox::default();
    let _wakes = counted(&mailbox);
    mailbox.post_frame(1, 1, &[1; 4]);
    let first = mailbox.take_frame(Vec::with_capacity(64)).unwrap();
    assert_eq!(first.rgba.len(), 4);
    mailbox.post_frame(1, 1, &[2; 4]);
    let second = mailbox.take_frame(first.rgba).unwrap();
    assert_eq!(second.rgba, vec![2; 4]);
}

/// The race the old flag-then-lock order had, interleaved by hand: a frame is stored
/// (the engine is between its two halves) when the window collects, and the wake that
/// follows finds nothing left. It must not return the spare buffer as a picture, or an
/// old picture of another size.
#[test]
fn a_frame_stored_between_the_flag_and_the_wake_is_taken_once() {
    let mailbox = Mailbox::default();
    let wakes = counted(&mailbox);
    mailbox.post_frame(2, 1, &[1; 8]);
    let first = mailbox.take_frame(vec![0xEE; 8]).unwrap();
    assert_eq!(first.rgba, vec![1; 8]);

    // The engine has stored a larger frame but not announced it yet.
    mailbox.store_frame(3, 1, &[2; 12]);
    let taken = mailbox.take_frame(vec![0xAA; 8]).unwrap();
    assert_eq!((taken.width, taken.height), (3, 1));
    assert_eq!(
        taken.rgba,
        vec![2; 12],
        "the new picture, not the spare buffer"
    );

    // Now it announces: the wake finds the picture already collected.
    mailbox.announce_frame();
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
    assert!(mailbox.take_frame(vec![0xBB; 8]).is_none());
    // And the next frame still wakes the window.
    mailbox.post_frame(3, 1, &[3; 12]);
    assert_eq!(wakes.load(Ordering::SeqCst), 3);
    assert_eq!(mailbox.take_frame(Vec::new()).unwrap().rgba, vec![3; 12]);
}

#[test]
fn a_picture_is_never_taken_twice() {
    let mailbox = Mailbox::default();
    let _wakes = counted(&mailbox);
    mailbox.post_frame(1, 1, &[7; 4]);
    assert!(mailbox.take_frame(Vec::new()).is_some());
    mailbox.announce_frame();
    assert!(mailbox.take_frame(vec![0xCC; 4]).is_none());
}

/// A poster and a collector racing for real: every picture taken is whole, whatever
/// its size, and the newest one is always collected in the end.
#[test]
fn concurrent_posts_and_takes_only_ever_give_whole_pictures() {
    const LAST: u32 = 2000;
    let mailbox = Arc::new(Mailbox::default());
    let _wakes = counted(&mailbox);
    let poster = {
        let mailbox = Arc::clone(&mailbox);
        std::thread::spawn(move || {
            for n in 1..=LAST {
                let width = 1 + n % 5;
                let rgba = vec![(n % 251) as u8; (width * 4) as usize];
                mailbox.post_frame(width, 1, &rgba);
            }
        })
    };
    let mut spare = Vec::new();
    let mut newest = 0u8;
    while !poster.is_finished() {
        if let Some(frame) = mailbox.take_frame(std::mem::take(&mut spare)) {
            assert_eq!(frame.rgba.len(), (frame.width * frame.height * 4) as usize);
            assert!(
                frame.rgba.iter().all(|b| *b == frame.rgba[0]),
                "a torn picture"
            );
            newest = frame.rgba[0];
            spare = frame.rgba;
        }
    }
    poster.join().unwrap();
    if let Some(frame) = mailbox.take_frame(spare) {
        newest = frame.rgba[0];
    }
    assert_eq!(u32::from(newest), LAST % 251);
}
