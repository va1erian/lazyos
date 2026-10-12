#![forbid(unsafe_code)]

//! Picture thumbnails for the icon view, loaded in the background.
//!
//! Nothing here blocks the UI thread. The view's paint hook asks
//! [`Thumbs::paint`] for each visible picture tile: a cached thumbnail is
//! drawn, anything else falls back to the generic picture icon and, the first
//! time, queues a job. Worker threads take jobs newest first (what the
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
/// Reader threads. A share's reads are latency-bound, so several in flight
/// fill the pipe; decoding is short next to that.
const WORKERS: usize = 4;
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
    /// Unreadable or not a picture; not asked for again until the folder is
    /// shown again.
    Failed,
}

/// What the workers share with the UI thread.
struct Shared {
    work: Mutex<Work>,
    wake_worker: Condvar,
    done: Mutex<Vec<(Key, Option<Image>)>>,
    /// Whether the UI was woken and has not drained since.
    notified: AtomicBool,
    reader: ThumbReader,
    wake_ui: Arc<dyn Fn() + Send + Sync>,
}

#[derive(Default)]
struct Work {
    queue: VecDeque<Key>,
    closed: bool,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn worker(&self) {
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
            let image = (self.reader)(&key.path, MAX_BYTES)
                .ok()
                .and_then(|bytes| thumb_decode::thumbnail(&bytes));
            locked(&self.done).push((key, image));
            // One wake per burst: the UI clears the flag when it drains.
            if !self.notified.swap(true, Ordering::AcqRel) {
                (self.wake_ui)();
            }
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
            Some(Slot::Pending | Slot::Failed) => true,
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
        for index in 0..WORKERS {
            let shared = Arc::clone(&self.shared);
            // A failed spawn leaves tiles on the generic icon.
            let _ = std::thread::Builder::new()
                .name(format!("thumbs-{index}"))
                .spawn(move || shared.worker());
        }
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
        for (key, image) in done {
            match image {
                Some(image) => {
                    changed = true;
                    order.push_back(key.clone());
                    cache.insert(key, Slot::Ready(image));
                }
                None => {
                    cache.insert(key, Slot::Failed);
                }
            }
        }
        while order.len() > CACHE_CAP {
            if let Some(oldest) = order.pop_front() {
                cache.remove(&oldest);
            }
        }
        changed
    }

    /// The window shows another folder or refreshed: forget queued work (the
    /// tiles it was for are gone) and let failures be retried. Made
    /// thumbnails stay, so going Back is instant.
    pub fn folder_changed(&self) {
        let forgotten = std::mem::take(&mut locked(&self.shared.work).queue);
        let mut cache = self.cache.borrow_mut();
        for key in forgotten {
            cache.remove(&key);
        }
        cache.retain(|_, slot| !matches!(slot, Slot::Failed));
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
mod tests {
    use std::ffi::OsString;
    use std::io;
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    use xui_canvas::Surface;
    use xui_core::theme::Theme;

    use super::*;
    use crate::platform::{Meta, RawEntry};

    fn png(side: u32, shade: u8) -> Vec<u8> {
        Image::from_rgba(
            side,
            side,
            [shade, 0, 0, 255].repeat((side * side) as usize),
        )
        .unwrap()
        .encode_png()
        .unwrap()
    }

    fn picture(name: &str, size: u64) -> Entry {
        let mut meta = Meta::bare(Path::new(name), Kind::File);
        meta.size = Some(size);
        Entry::from_raw(RawEntry {
            name: OsString::from(name),
            meta,
        })
    }

    struct Rig {
        thumbs: Thumbs,
        wakes: Arc<AtomicUsize>,
    }

    fn rig(reader: ThumbReader) -> Rig {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let wake = Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        Rig {
            thumbs: Thumbs::new(reader, wake),
            wakes,
        }
    }

    /// Paints `entry` on a fresh surface; returns whether it claimed the tile.
    fn paint(thumbs: &Thumbs, entry: &Entry) -> bool {
        let rect = Rect::new(4, 4, 52, 52);
        let mut surface = Surface::new(56, 56);
        let _ = Theme::light();
        surface.with_canvas_at(rect, 96, |canvas| {
            thumbs.paint(Path::new("/pics"), entry, canvas, rect)
        })
    }

    fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(10);
        while !condition() {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_thumbnail_arrives_in_the_background_and_then_draws() {
        let rig = rig(Arc::new(|_, _| Ok(png(300, 200))));
        let entry = picture("cat.png", 1000);
        assert!(
            !paint(&rig.thumbs, &entry),
            "first paint is the generic icon"
        );
        wait_for("the wake", || rig.wakes.load(Ordering::SeqCst) > 0);
        assert!(rig.thumbs.drain(), "a made thumbnail asks for a repaint");
        assert_eq!(rig.thumbs.ready(), 1);
        assert!(paint(&rig.thumbs, &entry), "now the thumbnail is drawn");
    }

    #[test]
    fn paint_does_not_wait_for_a_slow_reader() {
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let rig = rig(Arc::new(move |_, _| {
            let _ = locked(&gate).recv_timeout(Duration::from_secs(10));
            Ok(png(32, 9))
        }));
        let entry = picture("slow.png", 100);
        let started = Instant::now();
        for _ in 0..50 {
            assert!(!paint(&rig.thumbs, &entry));
        }
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(release);
    }

    #[test]
    fn a_failure_is_remembered_until_the_folder_is_shown_again() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let rig = rig(Arc::new(move |_, _| {
            seen.fetch_add(1, Ordering::SeqCst);
            Err(io::Error::other("share went away"))
        }));
        let entry = picture("gone.png", 100);
        paint(&rig.thumbs, &entry);
        wait_for("the failure", || rig.wakes.load(Ordering::SeqCst) > 0);
        assert!(!rig.thumbs.drain(), "a failure changes nothing on screen");
        for _ in 0..5 {
            paint(&rig.thumbs, &entry);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "not retried per paint");
        rig.thumbs.folder_changed();
        paint(&rig.thumbs, &entry);
        wait_for("the retry", || calls.load(Ordering::SeqCst) == 2);
    }

    #[test]
    fn non_pictures_and_oversized_files_are_never_read() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let rig = rig(Arc::new(move |_, _| {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }));
        for entry in [
            picture("notes.txt", 100),
            picture("huge.png", MAX_BYTES + 1),
            picture("empty.png", 0),
            picture("vector.svg", 100),
        ] {
            assert!(!paint(&rig.thumbs, &entry));
        }
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn the_queue_is_bounded_and_serves_the_newest_first() {
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let order = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
        let log = Arc::clone(&order);
        let rig = rig(Arc::new(move |path, _| {
            locked(&log).push(path.to_path_buf());
            let _ = locked(&gate).recv_timeout(Duration::from_secs(10));
            Ok(png(16, 1))
        }));
        // Every worker picks up a job and block on the gate; the rest queue.
        let total = QUEUE_CAP + 40;
        for index in 0..total {
            paint(&rig.thumbs, &picture(&format!("{index:03}.png"), 100));
            if index == WORKERS - 1 {
                wait_for("both workers busy", || locked(&order).len() == WORKERS);
            }
        }
        assert_eq!(locked(&rig.thumbs.shared.work).queue.len(), QUEUE_CAP);
        // Release exactly one job: the next one taken is the newest.
        release.send(()).unwrap();
        wait_for("a third read", || locked(&order).len() == WORKERS + 1);
        let third = locked(&order)[WORKERS].clone();
        assert_eq!(
            third,
            Path::new("/pics").join(format!("{:03}.png", total - 1))
        );
        drop(release);
        // Dropping the window's thumbnails stops the workers.
        drop(rig);
    }

    #[test]
    fn a_picture_is_drawn_inside_its_tile_without_distortion() {
        let rig = rig(Arc::new(|_, _| {
            let wide = Image::from_rgba(80, 20, [255, 0, 0, 255].repeat(1600)).unwrap();
            Ok(wide.encode_png().unwrap())
        }));
        let entry = picture("wide.png", 100);
        paint(&rig.thumbs, &entry);
        wait_for("the wake", || rig.wakes.load(Ordering::SeqCst) > 0);
        rig.thumbs.drain();
        let rect = Rect::new(0, 0, 48, 48);
        let mut surface = Surface::new(48, 48);
        surface.fill(xui_core::Color::rgb(255, 255, 255));
        surface.with_canvas_at(rect, 96, |canvas| {
            assert!(rig.thumbs.paint(Path::new("/pics"), &entry, canvas, rect));
        });
        let pixels = surface.pixels();
        let at = |x: usize, y: usize| pixels[(y * 48 + x) * 4..(y * 48 + x) * 4 + 4].to_vec();
        let white = vec![255u8; 4];
        assert_eq!(at(24, 2), white, "above the 4:1 picture stays background");
        assert_eq!(at(24, 46), white, "and below");
        assert_ne!(at(24, 24), white, "the picture is in the middle");
    }
}
