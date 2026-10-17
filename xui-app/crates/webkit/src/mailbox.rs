//! The hand-over between the engine's thread and the window's.
//!
//! WebKit runs its run loop on one thread and the window runs on another (a
//! Messenger handle belongs to the thread that made it, so the window cannot
//! move). The engine posts into a [`Mailbox`]; the window is woken once per
//! burst (a [`Notice`] through the app's `Proxy`) and collects the newest of
//! each. A frame the window has not collected yet is replaced, never queued:
//! only the latest picture matters, and the buffer is reused.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use crate::damage::{IRect, Scroll};
use crate::events::{Event, Events};

/// What the engine reports about the page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageState {
    /// The address on show: the page being loaded, or the one that is committed.
    pub url: String,
    /// The address of the page that is on screen (what its scripts and links belong to); empty
    /// before the first page commits.
    pub committed_url: String,
    /// The committed page came over HTTPS with a verified certificate (a failed verification is
    /// an error, never a page, so a committed `https:` page is a secure one).
    pub secure: bool,
    pub title: String,
    pub loading: bool,
    pub can_back: bool,
    pub can_forward: bool,
    /// Load progress in thousandths.
    pub progress: u32,
    /// What the status line says about a load that failed; empty when it did not.
    pub error: String,
    /// The page's process is gone: the page on show is blanked until the next load.
    pub crashed: bool,
    /// How many navigations have finished loading (`didFinishNavigation`) since the engine
    /// started. A load that ends without this growing was stopped, failed, or became a download.
    pub finished: u32,
}

/// A painted frame: top-down RGBA, opaque.
#[derive(Debug, Default)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// A rectangle of the picture, repainted: top-down RGBA, opaque, `rect.width() * rect.height()`
/// pixels.
#[derive(Debug)]
pub struct Patch {
    pub rect: IRect,
    pub rgba: Vec<u8>,
}

/// A change to the picture the window already has, from a frame of `size`: the part of it inside
/// `scroll.area` moves by the offset, then the patches are laid over it, in order.
#[derive(Debug)]
pub struct Delta {
    pub size: (u32, u32),
    pub scroll: Option<Scroll>,
    pub patches: Vec<Patch>,
}

impl Delta {
    fn bytes(&self) -> usize {
        self.patches.iter().map(|patch| patch.rgba.len()).sum()
    }
}

/// What the window collects: a whole new picture (if one was posted) and then the deltas posted
/// after it, oldest first. Taken together under one lock, so the deltas always belong to the
/// picture they follow.
#[derive(Debug, Default)]
pub struct Update {
    pub full: Option<Frame>,
    pub deltas: Vec<Delta>,
    /// The main frame's scroll offset (page pixels, vertical) the newest of these pixels were
    /// painted at, when the engine said (`set_scroll_offset`).
    pub scroll_y: Option<i32>,
}

/// Deltas the window may be behind by: past this (or past a frame's worth of pixels) the engine
/// is told to send a whole frame instead, which is no more work than applying them.
const MAX_PENDING_DELTAS: usize = 16;

/// Why the window should look at the mailbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    Frame,
    State,
    /// [`Mailbox::take_events`] has something.
    Events,
}

type Notify = Box<dyn Fn(Notice) + Send + Sync>;

/// The frame and whether it is one the window has not collected. `fresh` lives under the
/// frame's lock, with the pixels it describes: `take_frame` swaps the buffer out only when
/// `fresh` is set, so a wake that finds the picture already collected (a frame stored between
/// another `take_frame`'s flag and its lock) gets `None`, never the previous picture or the
/// spare buffer it handed in.
#[derive(Default)]
struct Slot {
    frame: Frame,
    fresh: bool,
    /// Deltas posted since `frame` that the window has not collected, oldest first.
    deltas: Vec<Delta>,
    delta_bytes: usize,
    /// The window has (or is about to get) a whole picture to apply deltas to. False after a
    /// delta was refused or the window gave its picture up: deltas are answered with "send a
    /// whole frame" until one arrives.
    has_base: bool,
    /// The offset the engine named for the post it is about to make, and the one of the newest
    /// post the window has not collected. Moved together under this lock, so an offset always
    /// goes with the pixels it was named for.
    staged_scroll: Option<i32>,
    scroll_y: Option<i32>,
}

#[derive(Default)]
pub struct Mailbox {
    frame: Mutex<Slot>,
    state: Mutex<PageState>,
    frame_waiting: AtomicBool,
    state_waiting: AtomicBool,
    events: Events,
    notify: Mutex<Option<Notify>>,
}

/// The process's mailbox: the engine's C ABI posts into it, the window attaches to it.
pub fn global() -> &'static Arc<Mailbox> {
    static MAILBOX: OnceLock<Arc<Mailbox>> = OnceLock::new();
    MAILBOX.get_or_init(|| Arc::new(Mailbox::default()))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock only means a thread panicked while holding it; the data
    // is a picture or a few strings and stays usable.
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Mailbox {
    /// Who to wake when something arrives (the window installs this once).
    pub fn set_notify(&self, notify: impl Fn(Notice) + Send + Sync + 'static) {
        *lock(&self.notify) = Some(Box::new(notify));
        // Whatever arrived before the window existed.
        if self.frame_waiting.load(Ordering::Acquire) {
            self.wake(Notice::Frame);
        }
        if self.state_waiting.load(Ordering::Acquire) {
            self.wake(Notice::State);
        }
        if self.events.pending() {
            self.wake(Notice::Events);
        }
    }

    /// The engine has news for the window (a download, a dialog, the link under the pointer).
    pub fn post_event(&self, event: Event) {
        if self.events.push(event) {
            self.wake(Notice::Events);
        }
    }

    /// Everything the engine reported since the last call, oldest first.
    pub fn take_events(&self) -> Vec<Event> {
        self.events.take()
    }

    fn wake(&self, notice: Notice) {
        if let Some(notify) = lock(&self.notify).as_ref() {
            notify(notice);
        }
    }

    /// The engine painted a frame (`rgba` is `width * height * 4` bytes).
    pub fn post_frame(&self, width: u32, height: u32, rgba: &[u8]) {
        self.store_frame(width, height, rgba);
        self.announce_frame();
    }

    /// The first half of [`post_frame`](Self::post_frame): the picture, marked fresh.
    fn store_frame(&self, width: u32, height: u32, rgba: &[u8]) {
        let mut slot = lock(&self.frame);
        slot.frame.width = width;
        slot.frame.height = height;
        slot.frame.rgba.clear();
        slot.frame.rgba.extend_from_slice(rgba);
        slot.fresh = true;
        // A whole picture supersedes every delta before it, and gives later ones a base.
        slot.deltas.clear();
        slot.delta_bytes = 0;
        slot.has_base = true;
        if let Some(y) = slot.staged_scroll.take() {
            slot.scroll_y = Some(y);
        }
    }

    /// The engine repainted part of the page: `delta` applies to the picture the window has.
    /// Returns true when it was not queued and the engine must send a whole frame next (the
    /// window has no picture to apply it to, is too far behind, or the sizes disagree).
    pub fn post_delta(&self, delta: Delta) -> bool {
        {
            let mut slot = lock(&self.frame);
            let size = (slot.frame.width, slot.frame.height);
            let frame_bytes = size.0 as usize * size.1 as usize * 4;
            if !slot.has_base || delta.size != size {
                slot.has_base = false;
                return true;
            }
            let bytes = delta.bytes();
            if slot.deltas.len() >= MAX_PENDING_DELTAS || slot.delta_bytes + bytes > frame_bytes {
                slot.deltas.clear();
                slot.delta_bytes = 0;
                slot.has_base = false;
                return true;
            }
            slot.delta_bytes += bytes;
            slot.deltas.push(delta);
            if let Some(y) = slot.staged_scroll.take() {
                slot.scroll_y = Some(y);
            }
        }
        self.announce_frame();
        false
    }

    /// The main frame's scroll offset of the pixels the engine posts next (the engine says it
    /// just before posting): it goes with that post (the engine names one before every post, so a
    /// refused post leaves nothing stale).
    pub fn set_scroll_offset(&self, y: i32) {
        lock(&self.frame).staged_scroll = Some(y);
    }

    /// The window cannot apply deltas to what it shows (too many parts, a refused delta): the
    /// next delta the engine posts is answered with "send a whole frame".
    pub fn request_full(&self) {
        lock(&self.frame).has_base = false;
    }

    /// The second half: wake the window unless a wake is already on its way.
    fn announce_frame(&self) {
        if !self.frame_waiting.swap(true, Ordering::AcqRel) {
            self.wake(Notice::Frame);
        }
    }

    /// The engine's page state changed.
    pub fn post_state(&self, state: PageState) {
        *lock(&self.state) = state;
        if !self.state_waiting.swap(true, Ordering::AcqRel) {
            self.wake(Notice::State);
        }
    }

    /// What arrived since the last call: the newest whole picture, if there is one, and the
    /// deltas posted after it. `spare` is the previous picture's buffer, handed back to be reused
    /// for the next whole picture.
    pub fn take_update(&self, spare: Vec<u8>) -> Option<Update> {
        let mut slot = lock(&self.frame);
        // Cleared under the lock `store_frame` takes: a frame stored after this point sets
        // the flag again and wakes the window, one stored before it is the one taken below.
        self.frame_waiting.store(false, Ordering::Release);
        if !slot.fresh && slot.deltas.is_empty() {
            return None;
        }
        let full = if std::mem::take(&mut slot.fresh) {
            let rgba = std::mem::replace(&mut slot.frame.rgba, spare);
            Some(Frame {
                width: slot.frame.width,
                height: slot.frame.height,
                rgba,
            })
        } else {
            None
        };
        slot.delta_bytes = 0;
        Some(Update {
            full,
            deltas: std::mem::take(&mut slot.deltas),
            scroll_y: slot.scroll_y.take(),
        })
    }

    /// The newest whole picture only (what the tests of the hand-over are about).
    #[cfg(test)]
    fn take_frame(&self, spare: Vec<u8>) -> Option<Frame> {
        self.take_update(spare).and_then(|update| update.full)
    }

    /// The newest page state, if it changed since the last call.
    pub fn take_state(&self) -> Option<PageState> {
        if !self.state_waiting.swap(false, Ordering::AcqRel) {
            return None;
        }
        Some(lock(&self.state).clone())
    }
}

#[cfg(test)]
mod tests {
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
}
