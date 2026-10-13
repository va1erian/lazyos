#![forbid(unsafe_code)]

//! Picture thumbnails for the icon view, loaded in the background.
//!
//! Nothing here blocks the UI thread. The view's paint hook asks
//! [`Thumbs::paint`] for each visible picture tile: a cached thumbnail is
//! drawn, anything else falls back to the generic picture icon and, the first
//! time, queues a job. Reader threads take jobs newest first (what the
//! user just scrolled to is wanted before what they scrolled past), read the
//! file through the platform's [`ThumbReader`] (a mounted network share is
//! just a path, and a slow read only holds up a worker), decode and shrink it
//! (`thumb_decode`), and post the result back. The window is woken once per
//! burst of results; [`Thumbs::drain`] then moves them into the cache and the
//! window repaints, so thumbnails appear one by one.
//!
//! Memory is bounded three ways: the queue holds at most [`QUEUE_CAP`] jobs
//! (older ones are forgotten and asked for again if they are painted again),
//! at most [`CACHE_CAP`] thumbnails are kept, and a file over [`MAX_BYTES`]
//! or [`thumb_decode::MAX_PIXELS`](super::thumb_decode::MAX_PIXELS) is never
//! decoded.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use xui_core::Color;
use xui_core::backend::Canvas;
use xui_core::geometry::Rect;
use xui_core::image::Image;

use super::entry::Entry;
use super::thumb_decode;
use crate::platform::{Kind, ThumbReader};

/// The largest file a thumbnail is made from. Reading it is the cost on a
/// network share, so a bigger picture keeps its generic icon.
pub const MAX_BYTES: u64 = 8 * 1024 * 1024;
/// Jobs waiting for a worker; the oldest are dropped past this.
const QUEUE_CAP: usize = 96;
/// Thumbnails kept (about 64 KiB each at most).
const CACHE_CAP: usize = 512;
/// Reader threads. A share's reads are latency-bound and cost the CPU almost
/// nothing, so several in flight fill the pipe.
const READERS: usize = 4;
/// Files read and waiting for the decoder; readers wait for room past this
/// (each holds up to [`MAX_BYTES`]).
const BACKLOG: usize = 3;
/// Space kept between a thumbnail and its tile's edge.
const MARGIN: i32 = 2;

/// What a thumbnail is of: the file and the state it was read in, so an
/// edited or replaced picture is a new key and is made again.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    path: PathBuf,
    size: u64,
    modified: Option<SystemTime>,
}

impl Key {
    /// The key for `entry` of folder `dir`, or `None` when the entry is not
    /// a picture worth decoding. Symlinks are skipped: a link may lead
    /// anywhere, including somewhere slow.
    fn of(dir: &Path, entry: &Entry) -> Option<Key> {
        let size = entry.size?;
        if entry.kind != Kind::File
            || size == 0
            || size > MAX_BYTES
            || !thumb_decode::supported(&entry.name)
        {
            return None;
        }
        Some(Key {
            path: dir.join(&entry.name),
            size,
            modified: entry.modified,
        })
    }
}

/// Where a key stands in the UI thread's cache.
enum Slot {
    /// Queued or being decoded.
    Pending,
    /// Made.
    Ready(Image),
    /// Could not be read (a share went away): asked for again when the
    /// folder is shown again.
    Unreadable,
    /// Read, but not a picture or too large to decode: the file's key
    /// (size, mtime) is unchanged, so it stays refused.
    Refused,
}

/// What a worker made of a job.
enum Outcome {
    Ready(Image),
    Unreadable,
    Refused,
}

/// Decodes running at once in the whole process, all windows together: a
/// decode peaks near three times its picture's RGBA size, so this bounds the
/// transient memory. Each window has one decoder thread; the guest has one CPU,
/// and a lone CPU-bound task is what the scheduler's CPU guard demotes behind
/// the UI, where several smaller ones would each stay under its threshold and
/// together starve the window.
const DECODERS: usize = 1;

/// A claim on one of the [`DECODERS`] slots.
struct DecodePermit;

static DECODING: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());

impl DecodePermit {
    fn acquire() -> DecodePermit {
        let (running, freed) = &DECODING;
        let mut running = locked(running);
        while *running >= DECODERS {
            running = freed.wait(running).unwrap_or_else(PoisonError::into_inner);
        }
        *running += 1;
        DecodePermit
    }
}

impl Drop for DecodePermit {
    fn drop(&mut self) {
        *locked(&DECODING.0) -= 1;
        DECODING.1.notify_one();
    }
}

/// What the workers share with the UI thread.
struct Shared {
    work: Mutex<Work>,
    /// Readers wait here for a job (or the close).
    wake_worker: Condvar,
    /// The decoder waits here for a file.
    decode_ready: Condvar,
    /// Readers wait here for room in the decode backlog.
    decode_room: Condvar,
    done: Mutex<Vec<(Key, Outcome)>>,
    /// Whether the UI was woken and has not drained since.
    notified: AtomicBool,
    reader: ThumbReader,
    wake_ui: Arc<dyn Fn() + Send + Sync>,
}

#[derive(Default)]
struct Work {
    queue: VecDeque<Key>,
    /// Files read, waiting for the decoder.
    decode: VecDeque<(Key, Vec<u8>)>,
    closed: bool,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    /// Reads one picture's file. A big file's header comes first, so a photo
    /// over the pixel cap costs a few KiB on a share, not the file. `Err` is
    /// the verdict when there is nothing to decode.
    fn read(&self, key: &Key) -> Result<Vec<u8>, Outcome> {
        if key.size > thumb_decode::HEADER {
            match (self.reader)(&key.path, thumb_decode::HEADER) {
                Ok(prefix) if thumb_decode::refuses(&prefix) => return Err(Outcome::Refused),
                Ok(_) => {}
                Err(_) => return Err(Outcome::Unreadable),
            }
        }
        // One byte past the limit tells a file that grew.
        let Ok(bytes) = (self.reader)(&key.path, MAX_BYTES + 1) else {
            return Err(Outcome::Unreadable);
        };
        if bytes.len() as u64 > MAX_BYTES {
            return Err(Outcome::Refused);
        }
        Ok(bytes)
    }

    fn post(&self, key: Key, outcome: Outcome) {
        locked(&self.done).push((key, outcome));
        // One wake per burst: the UI clears the flag when it drains.
        if !self.notified.swap(true, Ordering::AcqRel) {
            (self.wake_ui)();
        }
    }

    /// A reader thread: takes the newest job, reads it, hands the bytes to
    /// the decoder.
    fn reader_loop(&self) {
        loop {
            let key = {
                let mut work = locked(&self.work);
                loop {
                    if work.closed {
                        return;
                    }
                    // Newest first: the tile just scrolled to.
                    if let Some(key) = work.queue.pop_back() {
                        break key;
                    }
                    work = self
                        .wake_worker
                        .wait(work)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            };
            match self.read(&key) {
                Err(outcome) => self.post(key, outcome),
                Ok(bytes) => {
                    let mut work = locked(&self.work);
                    while work.decode.len() >= BACKLOG && !work.closed {
                        work = self
                            .decode_room
                            .wait(work)
                            .unwrap_or_else(PoisonError::into_inner);
                    }
                    work.decode.push_back((key, bytes));
                    drop(work);
                    self.decode_ready.notify_one();
                }
            }
        }
    }

    /// The decoder thread: the one place a picture is decoded.
    fn decoder_loop(&self) {
        loop {
            let (key, bytes) = {
                let mut work = locked(&self.work);
                loop {
                    if work.closed {
                        return;
                    }
                    // Newest first, like the readers.
                    if let Some(job) = work.decode.pop_back() {
                        break job;
                    }
                    work = self
                        .decode_ready
                        .wait(work)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            };
            self.decode_room.notify_one();
            let permit = DecodePermit::acquire();
            // A decoder that panics on a hostile file must not take the
            // thread (and its Pending slot) with it; under `panic = "abort"`
            // the fuzz test is the defence.
            let made = std::panic::catch_unwind(|| thumb_decode::thumbnail(&bytes));
            drop(permit);
            drop(bytes);
            let outcome = match made {
                Ok(Some(image)) => Outcome::Ready(image),
                _ => Outcome::Refused,
            };
            self.post(key, outcome);
        }
    }
}

/// One window's thumbnails: the cache the paint hook reads and the queue the
/// workers serve. Lives on the UI thread; the workers see only [`Shared`].
pub struct Thumbs {
    shared: Arc<Shared>,
    cache: RefCell<HashMap<Key, Slot>>,
    /// Ready keys, oldest first, for eviction.
    order: RefCell<VecDeque<Key>>,
    started: Cell<bool>,
}

impl Thumbs {
    /// Thumbnails read through `reader`; `wake_ui` is called (from a worker
    /// thread) when results are waiting for [`drain`](Thumbs::drain).
    pub fn new(reader: ThumbReader, wake_ui: Arc<dyn Fn() + Send + Sync>) -> Thumbs {
        Thumbs {
            shared: Arc::new(Shared {
                work: Mutex::default(),
                wake_worker: Condvar::new(),
                decode_ready: Condvar::new(),
                decode_room: Condvar::new(),
                done: Mutex::default(),
                notified: AtomicBool::new(false),
                reader,
                wake_ui,
            }),
            cache: RefCell::default(),
            order: RefCell::default(),
            started: Cell::new(false),
        }
    }

    /// Draws `entry`'s thumbnail into `rect` and returns `true`, or returns
    /// `false` (draw the generic icon) while it is not ready, asking for it
    /// the first time. Called from the view's paint, so it never waits.
    pub fn paint(&self, dir: &Path, entry: &Entry, canvas: &mut dyn Canvas, rect: Rect) -> bool {
        let Some(key) = Key::of(dir, entry) else {
            return false;
        };
        let known = match self.cache.borrow().get(&key) {
            Some(Slot::Ready(image)) => return draw(canvas, image, rect),
            Some(Slot::Pending | Slot::Unreadable | Slot::Refused) => true,
            None => false,
        };
        if !known {
            self.request(key);
        }
        false
    }

    fn request(&self, key: Key) {
        self.start_workers();
        let mut cache = self.cache.borrow_mut();
        cache.insert(key.clone(), Slot::Pending);
        let mut work = locked(&self.shared.work);
        work.queue.push_back(key);
        while work.queue.len() > QUEUE_CAP {
            if let Some(forgotten) = work.queue.pop_front() {
                // Painted again, it is asked for again.
                cache.remove(&forgotten);
            }
        }
        drop(work);
        self.shared.wake_worker.notify_one();
    }

    fn start_workers(&self) {
        if self.started.replace(true) {
            return;
        }
        // A failed spawn leaves tiles on the generic icon.
        for index in 0..READERS {
            let shared = Arc::clone(&self.shared);
            let _ = std::thread::Builder::new()
                .name(format!("thumb-read-{index}"))
                .spawn(move || shared.reader_loop());
        }
        let shared = Arc::clone(&self.shared);
        let _ = std::thread::Builder::new()
            .name("thumb-decode".to_string())
            .spawn(move || shared.decoder_loop());
    }

    /// Takes in the results the workers posted. Returns whether the view
    /// should repaint.
    pub fn drain(&self) -> bool {
        // Clear before taking, so a result posted meanwhile wakes us again.
        self.shared.notified.store(false, Ordering::Release);
        let done = std::mem::take(&mut *locked(&self.shared.done));
        let mut changed = false;
        let mut cache = self.cache.borrow_mut();
        let mut order = self.order.borrow_mut();
        for (key, outcome) in done {
            let slot = match outcome {
                Outcome::Ready(image) => {
                    changed = true;
                    order.push_back(key.clone());
                    Slot::Ready(image)
                }
                Outcome::Unreadable => Slot::Unreadable,
                Outcome::Refused => Slot::Refused,
            };
            cache.insert(key, slot);
        }
        while order.len() > CACHE_CAP {
            if let Some(oldest) = order.pop_front() {
                cache.remove(&oldest);
            }
        }
        changed
    }

    /// The window shows another folder or refreshed: forget queued work (the
    /// tiles it was for are gone) and let unreadable files be retried. Made
    /// thumbnails stay, so going Back is instant.
    pub fn folder_changed(&self) {
        let (queued, read) = {
            let mut work = locked(&self.shared.work);
            (
                std::mem::take(&mut work.queue),
                std::mem::take(&mut work.decode),
            )
        };
        self.shared.decode_room.notify_all();
        let mut cache = self.cache.borrow_mut();
        for key in queued
            .into_iter()
            .chain(read.into_iter().map(|(key, _)| key))
        {
            cache.remove(&key);
        }
        cache.retain(|_, slot| !matches!(slot, Slot::Unreadable));
    }

    /// How many thumbnails are made (tests).
    #[cfg(test)]
    pub(crate) fn ready(&self) -> usize {
        self.order.borrow().len()
    }
}

impl Drop for Thumbs {
    fn drop(&mut self) {
        locked(&self.shared.work).closed = true;
        self.shared.wake_worker.notify_all();
        self.shared.decode_ready.notify_all();
        self.shared.decode_room.notify_all();
    }
}

/// Draws `image` centred in `rect`, as large as fits without distortion, with
/// a hairline frame. A picture smaller than the tile is enlarged to it, as an
/// icon would be.
fn draw(canvas: &mut dyn Canvas, image: &Image, rect: Rect) -> bool {
    let (width, height) = image.size();
    let room = rect.shrink(MARGIN);
    if room.is_empty() || width == 0 || height == 0 {
        return false;
    }
    let (room_w, room_h) = (i64::from(room.width()), i64::from(room.height()));
    let (w, h) = (i64::from(width), i64::from(height));
    // The fit that touches the tighter side.
    let (draw_w, draw_h) = if w * room_h > h * room_w {
        (room_w, (h * room_w / w).max(1))
    } else {
        ((w * room_h / h).max(1), room_h)
    };
    let (draw_w, draw_h) = (draw_w as i32, draw_h as i32);
    let left = room.left + (room.width() - draw_w) / 2;
    let top = room.top + (room.height() - draw_h) / 2;
    let target = Rect::new(left, top, left + draw_w, top + draw_h);
    canvas.draw_image(image, target);
    canvas.stroke_rect(target, Color::rgb(128, 128, 128), 1.0);
    true
}

#[cfg(test)]
mod tests;
