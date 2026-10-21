//! The hand-over from WebKit's main thread to the service thread: an [`EngineEvents`] that copies
//! what the engine says and queues it, never waits and never grows without bound.
//!
//! What describes the present is kept as the latest per view (a page's state, its scroll position,
//! its newest frame): the service has no use for a frame it was too slow to see, and holding a queue
//! of frames is exactly what a slow client must not cause. What happened once (a link click, a copy,
//! the loss of a view, a marker line) is kept in order, up to [`QUEUE_KEPT`], oldest dropped first.
//! The service thread calls [`EventQueue::drain`] when the optional wake-up has told it to.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use crate::config::clip;
use crate::engine::EngineEvents;
use crate::limits::{EVENT_TEXT_BYTES, QUEUE_KEPT};

/// The latest page state of a view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateNow {
    pub title: String,
    pub loading: bool,
    pub finished: i32,
    pub error: String,
}

/// A whole frame as the engine painted it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameNow {
    pub width: i32,
    pub height: i32,
    pub rgba: Vec<u8>,
}

/// Something that happened once, in arrival order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Once {
    LinkClicked { view: u64, url: String },
    ClipboardWrite { view: u64, text: String },
    Lost { view: u64, reason: i32 },
    Say(String),
}

/// Everything queued since the last drain.
#[derive(Debug, Default)]
pub struct Drained {
    pub states: BTreeMap<u64, StateNow>,
    pub scrolls: BTreeMap<u64, (i32, i32, bool)>,
    pub frames: BTreeMap<u64, FrameNow>,
    pub once: Vec<Once>,
}

impl Drained {
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
            && self.scrolls.is_empty()
            && self.frames.is_empty()
            && self.once.is_empty()
    }
}

#[derive(Default)]
struct Inner {
    states: BTreeMap<u64, StateNow>,
    scrolls: BTreeMap<u64, (i32, i32, bool)>,
    frames: BTreeMap<u64, FrameNow>,
    once: VecDeque<Once>,
    dropped: u64,
}

/// A wake-up the service thread installs: it is called after every event is queued, from the
/// engine's thread, and must not block.
type Wake = Box<dyn Fn() + Send + Sync>;
/// Where the user's memory charge comes from (the host's quota read).
type MemorySource = Box<dyn Fn() -> Option<(u64, u64)> + Send + Sync>;

/// What the queue keeps of a text the page controls (a URL it clicked, what it copied, its title):
/// at most [`EVENT_TEXT_BYTES`], cut at a character boundary *before* it is stored, because a page
/// with scripts on can make tens of megabytes of it and [`QUEUE_KEPT`] of those would be gigabytes.
/// Nothing is sent longer than that anyway (`service/events.rs`).
fn kept(text: &str) -> String {
    clip(text, EVENT_TEXT_BYTES).to_owned()
}

/// The queue the engine's callbacks fill and the service drains.
#[derive(Default)]
pub struct EventQueue {
    inner: Mutex<Inner>,
    wake: Option<Wake>,
    memory: Option<MemorySource>,
}

impl EventQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Call `wake` after every event is queued (a doorbell for the service thread).
    pub fn with_wake(mut self, wake: impl Fn() + Send + Sync + 'static) -> Self {
        self.wake = Some(Box::new(wake));
        self
    }

    /// Answer `user_memory` from `source`.
    pub fn with_memory(
        mut self,
        source: impl Fn() -> Option<(u64, u64)> + Send + Sync + 'static,
    ) -> Self {
        self.memory = Some(Box::new(source));
        self
    }

    /// Take everything queued, leaving it empty.
    pub fn drain(&self) -> Drained {
        let mut inner = self.lock();
        Drained {
            states: std::mem::take(&mut inner.states),
            scrolls: std::mem::take(&mut inner.scrolls),
            frames: std::mem::take(&mut inner.frames),
            once: inner.once.drain(..).collect(),
        }
    }

    /// Events of the ordered kind that were dropped because the queue was full, since the start.
    pub fn dropped(&self) -> u64 {
        self.lock().dropped
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A panic while the lock was held leaves plain data behind; keep serving.
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn push_once(&self, event: Once) {
        {
            let mut inner = self.lock();
            if inner.once.len() >= QUEUE_KEPT {
                inner.once.pop_front();
                inner.dropped += 1;
            }
            inner.once.push_back(event);
        }
        self.ring();
    }

    fn ring(&self) {
        if let Some(wake) = &self.wake {
            wake();
        }
    }
}

impl EngineEvents for EventQueue {
    fn state(&self, view: u64, title: &str, loading: bool, finished: i32, error: &str) {
        let now = StateNow {
            title: kept(title),
            loading,
            finished,
            error: kept(error),
        };
        self.lock().states.insert(view, now);
        self.ring();
    }

    fn frame(&self, view: u64, rgba: &[u8], width: i32, height: i32) {
        {
            let mut inner = self.lock();
            // Reuse the previous frame's allocation: frames arrive at the paint rate.
            let slot = inner.frames.entry(view).or_insert_with(|| FrameNow {
                width: 0,
                height: 0,
                rgba: Vec::new(),
            });
            slot.width = width;
            slot.height = height;
            slot.rgba.clear();
            slot.rgba.extend_from_slice(rgba);
        }
        self.ring();
    }

    fn scroll(&self, view: u64, x: i32, y: i32, at_bottom: bool) {
        self.lock().scrolls.insert(view, (x, y, at_bottom));
        self.ring();
    }

    fn link_clicked(&self, view: u64, url: &str) {
        self.push_once(Once::LinkClicked {
            view,
            url: kept(url),
        });
    }

    fn clipboard_write(&self, view: u64, text: &str) {
        self.push_once(Once::ClipboardWrite {
            view,
            text: kept(text),
        });
    }

    fn view_lost(&self, view: u64, reason: i32) {
        {
            // Whatever the dead view said last is no longer news.
            let mut inner = self.lock();
            inner.states.remove(&view);
            inner.scrolls.remove(&view);
            inner.frames.remove(&view);
        }
        self.push_once(Once::Lost { view, reason });
    }

    fn say(&self, line: &str) {
        self.push_once(Once::Say(kept(line)));
    }

    fn user_memory(&self) -> Option<(u64, u64)> {
        self.memory.as_ref().and_then(|source| source())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                Once::Lost { .. } => 0,
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
}
