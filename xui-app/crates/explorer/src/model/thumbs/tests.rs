#![forbid(unsafe_code)]

//! The thumbnail cache, queue and workers, headless.

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
fn an_unreadable_file_is_retried_when_the_folder_is_shown_again() {
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
    // Every worker picks up a job and blocks on the gate; the rest queue.
    let total = QUEUE_CAP + 40;
    for index in 0..total {
        paint(&rig.thumbs, &picture(&format!("{index:03}.png"), 100));
        if index == READERS - 1 {
            wait_for("every worker busy", || locked(&order).len() == READERS);
        }
    }
    assert_eq!(locked(&rig.thumbs.shared.work).queue.len(), QUEUE_CAP);
    // Release exactly one job: the next one taken is the newest.
    release.send(()).unwrap();
    wait_for("one more read", || locked(&order).len() == READERS + 1);
    let next = locked(&order)[READERS].clone();
    assert_eq!(
        next,
        Path::new("/pics").join(format!("{:03}.png", total - 1))
    );
    drop(release);
    // Dropping the window's thumbnails stops the workers: once the gate is
    // gone nothing is left running and the shared state is only ours.
    let shared = Arc::clone(&rig.thumbs.shared);
    drop(rig);
    wait_for("the workers to exit", || Arc::strong_count(&shared) == 1);
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

#[test]
fn a_refused_file_is_not_read_again_on_a_refresh() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let rig = rig(Arc::new(move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(b"not a picture".to_vec())
    }));
    let entry = picture("fake.png", 100);
    paint(&rig.thumbs, &entry);
    wait_for("the verdict", || rig.wakes.load(Ordering::SeqCst) > 0);
    rig.thumbs.drain();
    rig.thumbs.folder_changed();
    for _ in 0..5 {
        paint(&rig.thumbs, &entry);
    }
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// A photo over the pixel cap costs one small read, not the file.
#[test]
fn an_oversized_picture_is_refused_from_its_header() {
    let reads = Arc::new(Mutex::new(Vec::<u64>::new()));
    let log = Arc::clone(&reads);
    let rig = rig(Arc::new(move |_, max| {
        locked(&log).push(max);
        let mut header = png(4, 1);
        header[16..20].copy_from_slice(&20_000u32.to_be_bytes());
        header[20..24].copy_from_slice(&20_000u32.to_be_bytes());
        Ok(header)
    }));
    let entry = picture("photo.png", 6 * 1024 * 1024);
    paint(&rig.thumbs, &entry);
    wait_for("the verdict", || rig.wakes.load(Ordering::SeqCst) > 0);
    rig.thumbs.drain();
    assert_eq!(*locked(&reads), vec![thumb_decode::HEADER]);
}

#[test]
fn old_thumbnails_are_evicted_past_the_cap() {
    let rig = rig(Arc::new(|_, _| Ok(png(8, 1))));
    let total = CACHE_CAP + 20;
    let mut made = 0;
    for index in 0..total {
        paint(&rig.thumbs, &picture(&format!("{index:04}.png"), 100));
        // Keep the queue below its own cap so none is forgotten.
        if index % 32 == 31 || index == total - 1 {
            wait_for("a batch", || {
                rig.thumbs.drain();
                rig.thumbs
                    .cache
                    .borrow()
                    .values()
                    .all(|slot| !matches!(slot, Slot::Pending))
            });
            made = index + 1;
        }
    }
    assert_eq!(made, total);
    assert_eq!(rig.thumbs.ready(), CACHE_CAP);
    assert_eq!(rig.thumbs.cache.borrow().len(), CACHE_CAP);
}
