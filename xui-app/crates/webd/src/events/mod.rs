//! The hand-over from WebKit's main thread to the service thread: an [`EngineEvents`] that copies
//! what the engine says and queues it, never waits and never grows without bound.
//!
//! What describes the present is kept as the latest per view (a page's state, its scroll position,
//! its newest frame): the service has no use for a frame it was too slow to see, and holding a queue
//! of frames is exactly what a slow client must not cause. What happened once (a link click, a copy,
//! the loss of a view, a marker line) is kept in order, up to [`QUEUE_KEPT`](crate::limits::QUEUE_KEPT),
//! the oldest that nobody waits for dropped first (`once.rs`).
//! The service thread calls [`EventQueue::drain`] when the optional wake-up has told it to.
//!
//! Full views (P2 slice 3) add: their page state ([`PageNow`]), the pictures of a view with deltas
//! (`pictures`), bands, the link under the pointer, the pointer's shape and the page's
//! responsiveness (latest per view), the questions and news of `full` (in order), and the decisions
//! the engine's thread makes on the spot from the rules the service registered (`rules`).

mod full;
mod once;
pub mod pictures;
pub mod rules;

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use crate::config::clip;
use crate::engine::EngineEvents;
use crate::limits::{BANDS_KEPT, EVENT_TEXT_BYTES};
use pictures::{Pictures, Update};
use rules::Table;

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

/// The latest page state of a full view (`webd_page_state`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageNow {
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub can_back: bool,
    pub can_forward: bool,
    pub progress: u32,
    pub error: String,
    pub committed_url: String,
    pub secure: bool,
    pub finished: u32,
}

/// Rows of the page outside a view (an overscan band), RGBA, `width * 4` per row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BandNow {
    pub doc_x: i32,
    pub doc_y: i32,
    pub width: i32,
    pub height: i32,
    pub rgba: Vec<u8>,
}

/// Something that happened once, in arrival order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Once {
    LinkClicked {
        view: u64,
        url: String,
    },
    ClipboardWrite {
        view: u64,
        text: String,
    },
    Lost {
        view: u64,
        reason: i32,
    },
    Say(String),
    // ---- full views ----
    NewWindow {
        view: u64,
        url: String,
        gesture: bool,
    },
    /// The engine holds navigation `request` until the service answers (`Handoff.Ask`).
    Held {
        view: u64,
        request: u64,
        url: String,
        kind: i32,
        gesture: bool,
    },
    /// The page asked for an address another program opens (`Handoff.Browser`), and the engine did
    /// not load it: the service hands it over.
    HandOff {
        view: u64,
        url: String,
    },
    Dialog {
        view: u64,
        dialog: u64,
        kind: i32,
        message: String,
        default: String,
    },
    DownloadStarted {
        view: u64,
        download: u64,
        url: String,
        name: String,
        total: i64,
    },
    /// Coalesced: a newer report of the same download replaces one not yet drained.
    DownloadProgress {
        view: u64,
        download: u64,
        received: i64,
        total: i64,
    },
    DownloadDone {
        view: u64,
        download: u64,
        name: String,
    },
    DownloadFailed {
        view: u64,
        download: u64,
        reason: String,
    },
    NotifyShow {
        view: u64,
        id: u64,
        origin: String,
        title: String,
        body: String,
    },
    NotifyCancel {
        view: u64,
        id: u64,
    },
    NotifyGone {
        view: u64,
        id: u64,
    },
    NotifyAsk {
        view: u64,
        origin: String,
    },
    NotifyAskGone {
        view: u64,
        origin: String,
    },
    BlockingList {
        view: u64,
        list: String,
        status: i32,
    },
}

/// Everything queued since the last drain.
#[derive(Debug, Default)]
pub struct Drained {
    pub states: BTreeMap<u64, StateNow>,
    pub scrolls: BTreeMap<u64, (i32, i32, bool)>,
    pub frames: BTreeMap<u64, FrameNow>,
    pub once: Vec<Once>,
    pub pages: BTreeMap<u64, PageNow>,
    /// The pictures of views with deltas.
    pub pictures: BTreeMap<u64, Update>,
    pub bands: BTreeMap<u64, Vec<BandNow>>,
    pub hovers: BTreeMap<u64, String>,
    pub cursors: BTreeMap<u64, i32>,
    pub responsive: BTreeMap<u64, bool>,
    pub blocked: BTreeMap<u64, i32>,
}

impl Drained {
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
            && self.scrolls.is_empty()
            && self.frames.is_empty()
            && self.once.is_empty()
            && self.pages.is_empty()
            && self.pictures.is_empty()
            && self.bands.is_empty()
            && self.hovers.is_empty()
            && self.cursors.is_empty()
            && self.responsive.is_empty()
            && self.blocked.is_empty()
    }
}

#[derive(Default)]
struct Inner {
    states: BTreeMap<u64, StateNow>,
    scrolls: BTreeMap<u64, (i32, i32, bool)>,
    frames: BTreeMap<u64, FrameNow>,
    once: VecDeque<Once>,
    dropped: u64,
    pages: BTreeMap<u64, PageNow>,
    /// One entry per view with deltas, made when the service registers it.
    pictures: BTreeMap<u64, Pictures>,
    bands: BTreeMap<u64, VecDeque<BandNow>>,
    hovers: BTreeMap<u64, String>,
    cursors: BTreeMap<u64, i32>,
    responsive: BTreeMap<u64, bool>,
    blocked: BTreeMap<u64, i32>,
}

impl Inner {
    /// Forget everything about a view that is gone.
    fn forget(&mut self, view: u64) {
        self.states.remove(&view);
        self.scrolls.remove(&view);
        self.frames.remove(&view);
        self.pages.remove(&view);
        self.bands.remove(&view);
        self.hovers.remove(&view);
        self.cursors.remove(&view);
        self.responsive.remove(&view);
        self.blocked.remove(&view);
        if let Some(pictures) = self.pictures.get_mut(&view) {
            pictures.take();
        }
    }
}

/// A wake-up the service thread installs: it is called after every event is queued, from the
/// engine's thread, and must not block.
type Wake = Box<dyn Fn() + Send + Sync>;
/// Where the user's memory charge comes from (the host's quota read).
type MemorySource = Box<dyn Fn() -> Option<(u64, u64)> + Send + Sync>;

/// What the queue keeps of a text the page controls (a URL it clicked, what it copied, its title):
/// at most [`EVENT_TEXT_BYTES`], cut at a character boundary *before* it is stored, because a page
/// with scripts on can make tens of megabytes of it and `QUEUE_KEPT` of those would be gigabytes.
/// Nothing is sent longer than that anyway (`service/events.rs`).
fn kept(text: &str) -> String {
    clip(text, EVENT_TEXT_BYTES).to_owned()
}

/// The clock the input lock is read with: milliseconds, monotonic, never 0 after the start.
type Clock = Box<dyn Fn() -> u64 + Send + Sync>;

/// The queue the engine's callbacks fill and the service drains.
pub struct EventQueue {
    inner: Mutex<Inner>,
    /// The full views' rules, for the decisions on the engine's thread (`rules`).
    rules: Mutex<Table>,
    wake: Option<Wake>,
    memory: Option<MemorySource>,
    clock: Clock,
}

impl Default for EventQueue {
    fn default() -> Self {
        let start = Instant::now();
        EventQueue {
            inner: Mutex::default(),
            rules: Mutex::default(),
            wake: None,
            memory: None,
            clock: Box::new(move || start.elapsed().as_millis() as u64 + 1),
        }
    }
}

impl EventQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the time with `clock` (milliseconds; the service's `pump` clock, so an input the service
    /// stamped and a navigation the engine decides are on one time line).
    pub fn with_clock(mut self, clock: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    /// The time on the queue's clock.
    pub fn now_ms(&self) -> u64 {
        (self.clock)()
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
        let pictures = inner
            .pictures
            .iter_mut()
            .filter(|(_, pictures)| !pictures.is_empty())
            .map(|(view, pictures)| (*view, pictures.take()))
            .collect();
        Drained {
            states: std::mem::take(&mut inner.states),
            scrolls: std::mem::take(&mut inner.scrolls),
            frames: std::mem::take(&mut inner.frames),
            once: inner.once.drain(..).collect(),
            pages: std::mem::take(&mut inner.pages),
            pictures,
            bands: std::mem::take(&mut inner.bands)
                .into_iter()
                .map(|(view, bands)| (view, bands.into()))
                .collect(),
            hovers: std::mem::take(&mut inner.hovers),
            cursors: std::mem::take(&mut inner.cursors),
            responsive: std::mem::take(&mut inner.responsive),
            blocked: std::mem::take(&mut inner.blocked),
        }
    }

    /// The full views' rules, for the service to keep up to date.
    pub fn rules(&self) -> MutexGuard<'_, Table> {
        self.rules
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A view with deltas starts: its pictures are kept apart from whole frames, in order.
    pub fn track_pictures(&self, view: u64) {
        self.lock().pictures.insert(view, Pictures::default());
    }

    /// A view ended: its pictures and rules go.
    pub fn forget_view(&self, view: u64) {
        let mut inner = self.lock();
        inner.forget(view);
        inner.pictures.remove(&view);
        drop(inner);
        self.rules().unregister(view);
    }

    /// The client lost a delta (no free slot, a full queue): deltas are refused until a whole frame.
    pub fn lose_base(&self, view: u64) {
        if let Some(pictures) = self.lock().pictures.get_mut(&view) {
            pictures.lose_base();
        }
    }

    fn push_band(&self, view: u64, band: BandNow) {
        {
            let mut inner = self.lock();
            let bands = inner.bands.entry(view).or_default();
            if bands.len() >= BANDS_KEPT {
                bands.pop_front();
            }
            bands.push_back(band);
        }
        self.ring();
    }

    /// Events of the ordered kind that were dropped because the queue was full, since the start.
    pub fn dropped(&self) -> u64 {
        self.lock().dropped
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while the lock was held leaves plain data behind; keep serving.
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn push_once(&self, event: Once) {
        self.lock().push(event);
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
            // A view with deltas keeps its whole frames in order with its deltas.
            if let Some(pictures) = inner.pictures.get_mut(&view) {
                pictures.post_whole(rgba, width, height);
                drop(inner);
                self.ring();
                return;
            }
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
        {
            let mut inner = self.lock();
            // A view with deltas: the offset goes with the post it comes just before.
            if let Some(pictures) = inner.pictures.get_mut(&view) {
                pictures.stage_offset((x, y, at_bottom));
                return;
            }
            inner.scrolls.insert(view, (x, y, at_bottom));
        }
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
            self.lock().forget(view);
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
mod tests;
