//! The Video Player window driven offscreen: open the sample, play it to the
//! end, pause, seek, a file that is not a video; with snapshots
//! (`target/snapshots/video-*.png`) for a human to look at.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use xui_canvas::snapshot::{render_with, Snapshot, Stage};
use xui_core::{Dip, Image, Theme};
use xui_videoview::{Host, Msg, VideoApp, WINDOW};

const SAMPLE: &[u8] = include_bytes!("../../video/tests/data/sample.avi");

/// A file in a fresh temp folder, removed on drop.
struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str, bytes: &[u8]) -> TempFile {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("videoview-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        TempFile(path)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if let Some(dir) = self.0.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn register_fonts() {
    let fonts: [&[u8]; 2] = [
        include_bytes!("../../../../assets/fonts/DroidSans.ttf"),
        include_bytes!("../../../../assets/fonts/DroidSans-Bold.ttf"),
    ];
    for font in fonts {
        xui_canvas::add_font(font.to_vec());
    }
    xui_canvas::set_default_family("Droid Sans");
}

/// Runs `test` on its own thread with the fonts, failing if it hangs.
fn watchdog<T: Send + 'static>(test: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        register_fonts();
        let _ = tx.send(test());
    });
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(value) => {
            let _ = handle.join();
            value
        }
        Err(_) => match handle.join() {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(()) => panic!("the window test hung"),
        },
    }
}

type Log = Rc<RefCell<Vec<String>>>;

/// Builds the window, runs `step`, and returns the capture with the log.
fn drive(theme: Theme, step: impl FnOnce(&Stage<'_, Msg>, &Log) + 'static) -> (Image, Vec<String>) {
    let log: Log = Rc::default();
    let (writer, reader, probe) = (Rc::clone(&log), Rc::clone(&log), log);
    let image = render_with(
        Snapshot::new(Dip(WINDOW.0 as f32), Dip(WINDOW.1 as f32)).theme(theme),
        move |ui| {
            let mut host = Host::std(std::env::temp_dir());
            host.log = Rc::new(move |line| writer.borrow_mut().push(line.to_owned()));
            VideoApp::build(ui, host)
        },
        move |stage| step(stage, &probe),
    )
    .expect("the headless render");
    let log = reader.borrow().clone();
    (image, log)
}

/// Pumps the frame timer's message until `done` holds for the log.
fn until(stage: &Stage<'_, Msg>, log: &Log, what: &str, done: impl Fn(&[String]) -> bool) {
    let started = Instant::now();
    loop {
        stage.emit(Msg::Tick);
        if done(&log.borrow()) {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "timed out waiting for {what}: {:?}",
            log.borrow()
        );
        std::thread::sleep(Duration::from_millis(8));
    }
}

fn has(log: &[String], prefix: &str) -> bool {
    log.iter().any(|l| l.starts_with(prefix))
}

fn save(image: &Image, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

/// The share of pixels that are clearly coloured (the test pattern), not
/// grey chrome or black bars.
fn coloured(image: &Image) -> f32 {
    let px = image.pixels().as_chunks::<4>().0.iter();
    let total = px.len() as f32;
    let n = px
        .filter(|p| {
            let hi = p[0].max(p[1]).max(p[2]) as i32;
            let lo = p[0].min(p[1]).min(p[2]) as i32;
            hi - lo > 80
        })
        .count() as f32;
    n / total
}

#[test]
fn the_empty_window_says_how_to_open_a_video() {
    let (image, log) = watchdog(|| drive(Theme::dark(), |_, _| {}));
    save(&image, "video-empty.png");
    assert!(log.is_empty(), "{log:?}");
    assert!(coloured(&image) < 0.001);
}

#[test]
fn playing_the_sample_to_the_end_draws_it_and_reports_it() {
    let file = TempFile::new("sample.avi", SAMPLE);
    let path = file.0.clone();
    let (image, log) = watchdog(move || {
        drive(Theme::dark(), move |stage, log| {
            stage.emit(Msg::OpenChosen(path));
            until(stage, log, "the end", |l| has(l, "VIDEO:END:PASS"));
            stage.emit(Msg::Tick);
        })
    });
    save(&image, "video-played.png");
    assert!(
        log.iter().any(|l| l.starts_with("VIDEO:OPEN:PASS:")
            && l.ends_with("sample.avi:160x120:25.00:50:MJPG")),
        "{log:?}"
    );
    assert!(has(&log, "VIDEO:POS:"), "{log:?}");
    let end = log
        .iter()
        .find(|l| l.starts_with("VIDEO:END:PASS"))
        .unwrap();
    assert!(end.contains("shown="), "{end}");
    assert!(!has(&log, "VIDEO:ERROR"), "{log:?}");
    // The last frame stays on screen.
    assert!(coloured(&image) > 0.02, "no picture: {}", coloured(&image));
}

#[test]
fn pause_stops_the_position_reports_and_play_resumes_them() {
    let file = TempFile::new("sample.avi", SAMPLE);
    let path = file.0.clone();
    let (_, log) = watchdog(move || {
        drive(Theme::light(), move |stage, log| {
            stage.emit(Msg::OpenChosen(path));
            until(stage, log, "playback", |l| has(l, "VIDEO:POS:"));
            stage.emit(Msg::TogglePlay);
            // Let in-flight reports land, then watch a quiet stretch.
            for _ in 0..10 {
                stage.emit(Msg::Tick);
                std::thread::sleep(Duration::from_millis(20));
            }
            let quiet = log.borrow().len();
            for _ in 0..30 {
                stage.emit(Msg::Tick);
                std::thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(log.borrow().len(), quiet, "reports while paused");
            stage.emit(Msg::TogglePlay);
            until(stage, log, "VIDEO:PLAY", |l| has(l, "VIDEO:PLAY"));
        })
    });
    assert!(has(&log, "VIDEO:PAUSE"), "{log:?}");
    assert!(has(&log, "VIDEO:PLAY"), "{log:?}");
}

#[test]
fn the_seek_bar_and_the_arrow_keys_seek() {
    let file = TempFile::new("sample.avi", SAMPLE);
    let path = file.0.clone();
    let (image, log) = watchdog(move || {
        drive(Theme::dark(), move |stage, log| {
            stage.emit(Msg::OpenChosen(path));
            until(stage, log, "playback", |l| has(l, "VIDEO:POS:"));
            stage.emit(Msg::TogglePlay);
            stage.emit(Msg::Seek(500.0));
            until(stage, log, "the seek", |l| has(l, "VIDEO:SEEK:1000"));
            stage.emit(Msg::SeekBy(-500_000));
            until(stage, log, "the step", |l| has(l, "VIDEO:SEEK:500"));
            for _ in 0..20 {
                stage.emit(Msg::Tick);
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    });
    save(&image, "video-seeked.png");
    assert!(has(&log, "VIDEO:SEEK:1000"), "{log:?}");
    assert!(has(&log, "VIDEO:SEEK:500"), "{log:?}");
    assert!(coloured(&image) > 0.02);
}

#[test]
fn a_file_that_is_not_a_video_is_explained_not_played() {
    let file = TempFile::new("notes.avi", b"this is not a video file at all");
    let path = file.0.clone();
    let (image, log) = watchdog(move || {
        drive(Theme::dark(), move |stage, _| {
            stage.emit(Msg::OpenChosen(path));
            stage.emit(Msg::Tick);
        })
    });
    save(&image, "video-bad.png");
    assert!(has(&log, "VIDEO:OPEN:FAIL:"), "{log:?}");
    assert!(!has(&log, "VIDEO:OPEN:PASS"), "{log:?}");
    assert!(coloured(&image) < 0.02);
}

#[test]
fn closing_the_window_stops_playback_and_says_so() {
    let file = TempFile::new("sample.avi", SAMPLE);
    let path = file.0.clone();
    let (_, log) = watchdog(move || {
        drive(Theme::dark(), move |stage, log| {
            stage.emit(Msg::OpenChosen(path));
            until(stage, log, "playback", |l| has(l, "VIDEO:POS:"));
            stage.emit(Msg::Close);
        })
    });
    assert_eq!(log.last().map(String::as_str), Some("VIDEO:QUIT:PASS"));
}
