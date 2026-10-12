//! The hand-over between the engine's thread and the window's.
//!
//! WebKit runs its run loop on one thread and the window runs on another (a
//! Messenger handle belongs to the thread that made it, so the window cannot
//! move). The engine posts into a [`Mailbox`]; the window is woken once per
//! burst (a [`Notice`] through the app's `Proxy`) and collects the newest of
//! each. A frame the window has not collected yet is replaced, never queued:
//! only the latest picture matters, and the buffer is reused.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

/// What the engine reports about the page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageState {
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub can_back: bool,
    pub can_forward: bool,
    /// Load progress in thousandths.
    pub progress: u32,
}

/// A painted frame: top-down RGBA, opaque.
#[derive(Debug, Default)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Why the window should look at the mailbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    Frame,
    State,
}

type Notify = Box<dyn Fn(Notice) + Send + Sync>;

#[derive(Default)]
pub struct Mailbox {
    frame: Mutex<Frame>,
    state: Mutex<PageState>,
    frame_waiting: AtomicBool,
    state_waiting: AtomicBool,
    notify: Mutex<Option<Notify>>,
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
    }

    fn wake(&self, notice: Notice) {
        if let Some(notify) = lock(&self.notify).as_ref() {
            notify(notice);
        }
    }

    /// The engine painted a frame (`rgba` is `width * height * 4` bytes).
    pub fn post_frame(&self, width: u32, height: u32, rgba: &[u8]) {
        {
            let mut frame = lock(&self.frame);
            frame.width = width;
            frame.height = height;
            frame.rgba.clear();
            frame.rgba.extend_from_slice(rgba);
        }
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

    /// The newest frame, if one arrived since the last call. `spare` is the
    /// previous picture's buffer, handed back to be reused.
    pub fn take_frame(&self, spare: Vec<u8>) -> Option<Frame> {
        if !self.frame_waiting.swap(false, Ordering::AcqRel) {
            return None;
        }
        let mut frame = lock(&self.frame);
        let rgba = std::mem::replace(&mut frame.rgba, spare);
        Some(Frame {
            width: frame.width,
            height: frame.height,
            rgba,
        })
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
}
