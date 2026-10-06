//! The PDF Viewer window driven offscreen: open the sample, wait for its
//! tiles, zoom, move between pages, a locked file and a broken one, with
//! snapshots (`target/snapshots/pdf-*.png`) for a human to look at.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use xui_canvas::snapshot::{render_with, Snapshot, Stage};
use xui_core::backend::BackendError;
use xui_core::widget::DialogAction;
use xui_core::{Dip, Image, Theme};
use xui_pdfview::{Host, Msg, PdfApp, Viewer, WINDOW};

const SAMPLE: &[u8] = include_bytes!("../../pdf/testdata/sample.pdf");
const LOCKED: &[u8] = include_bytes!("../../pdf/testdata/sample-aes256.pdf");

/// A file in a fresh temp folder, removed on drop.
struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str, bytes: &[u8]) -> TempFile {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pdfview-{}-{n}", std::process::id()));
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

type Shared<T> = Rc<RefCell<T>>;

/// What a step can see: the viewer and the evidence lines so far.
struct Probe {
    viewer: Shared<Option<Rc<RefCell<Viewer>>>>,
    log: Shared<Vec<String>>,
    notes: Shared<Vec<String>>,
}

impl Probe {
    fn viewer(&self) -> Rc<RefCell<Viewer>> {
        Rc::clone(self.viewer.borrow().as_ref().expect("the app is built"))
    }

    fn note(&self, line: String) {
        self.notes.borrow_mut().push(line);
    }
}

/// Pump timer ticks until every requested tile has arrived.
fn settle(stage: &Stage<'_, Msg>, probe: &Probe) {
    let started = Instant::now();
    loop {
        stage.emit(Msg::Tick);
        if !probe.viewer().borrow().busy() {
            stage.emit(Msg::Tick);
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the tiles never arrived"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Builds the window, runs `step`, and returns the capture with the log and
/// the step's notes.
fn drive(
    theme: Theme,
    step: impl FnOnce(&Stage<'_, Msg>, &Probe) + 'static,
) -> (Image, Vec<String>, Vec<String>) {
    let probe = Probe {
        viewer: Rc::default(),
        log: Rc::default(),
        notes: Rc::default(),
    };
    let (slot, log) = (Rc::clone(&probe.viewer), Rc::clone(&probe.log));
    let (log_out, notes_out) = (Rc::clone(&probe.log), Rc::clone(&probe.notes));
    let image = render_with(
        Snapshot::new(Dip(WINDOW.0 as f32), Dip(WINDOW.1 as f32)).theme(theme),
        move |ui| {
            let mut host = Host::std(std::env::temp_dir());
            host.threads = 2;
            host.log = Rc::new(move |line| log.borrow_mut().push(line.to_owned()));
            let app = PdfApp::build(ui, host).map_err(|e: BackendError| e)?;
            *slot.borrow_mut() = Some(app.viewer_handle());
            Ok(app)
        },
        move |stage| step(stage, &probe),
    )
    .expect("the headless render");
    let log = log_out.borrow().clone();
    let notes = notes_out.borrow().clone();
    (image, log, notes)
}

fn save(image: &Image, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

/// The share of `image`'s pixels that are near white, and near black.
fn ink(image: &Image) -> (f32, f32) {
    let px = image.pixels().as_chunks::<4>().0.iter();
    let total = px.len() as f32;
    let white = px
        .clone()
        .filter(|p| p[0] > 245 && p[1] > 245 && p[2] > 245)
        .count() as f32;
    let dark = px.filter(|p| p[0] < 60 && p[1] < 60 && p[2] < 60).count() as f32;
    (white / total, dark / total)
}

#[test]
fn opening_the_sample_draws_its_first_page() {
    let file = TempFile::new("lazyos-sample.pdf", SAMPLE);
    let path = file.0.clone();
    let (image, log, notes) = watchdog(move || {
        drive(Theme::light(), move |stage, probe| {
            stage.emit(Msg::OpenChosen(path));
            settle(stage, probe);
            let v = probe.viewer();
            let v = v.borrow();
            probe.note(format!(
                "pages={} current={}",
                v.page_count(),
                v.current_page()
            ));
            probe.note(format!("cached={}", v.cache.len()));
        })
    });
    save(&image, "pdf-open.png");
    assert!(
        log.iter()
            .any(|l| l.starts_with("PDF:OPEN:PASS:") && l.ends_with("lazyos-sample.pdf:6")),
        "{log:?}"
    );
    assert!(
        log.iter().any(|l| l.starts_with("PDF:PAGE:DRAWN:1:")),
        "{log:?}"
    );
    assert_eq!(notes[0], "pages=6 current=0", "{notes:?}");
    let (white, dark) = ink(&image);
    assert!(white > 0.4, "the page fills most of the window: {white}");
    assert!(dark > 0.002, "the title page's text is drawn: {dark}");
}

#[test]
fn zooming_and_moving_between_pages() {
    let file = TempFile::new("sample.pdf", SAMPLE);
    let path = file.0.clone();
    let (image, log, notes) = watchdog(move || {
        drive(Theme::dark(), move |stage, probe| {
            stage.emit(Msg::OpenChosen(path));
            settle(stage, probe);
            let factor = |probe: &Probe| probe.viewer().borrow().factor();
            let fit_width = factor(probe);
            stage.emit(Msg::ActualSize);
            probe.note(format!("actual={:.2}", factor(probe)));
            stage.emit(Msg::ZoomStep(true));
            probe.note(format!("in={:.2}", factor(probe)));
            stage.emit(Msg::ZoomStep(false));
            stage.emit(Msg::ZoomStep(false));
            probe.note(format!("out={:.2}", factor(probe)));
            stage.emit(Msg::FitWidth);
            probe.note(format!(
                "width_again={}",
                (factor(probe) - fit_width).abs() < 1e-4
            ));
            stage.emit(Msg::NextPage);
            stage.emit(Msg::NextPage);
            probe.note(format!("page={}", probe.viewer().borrow().current_page()));
            stage.emit(Msg::LastPage);
            probe.note(format!("last={}", probe.viewer().borrow().current_page()));
            stage.emit(Msg::FirstPage);
            stage.emit(Msg::FitPage);
            stage.emit(Msg::GoToPage(3));
            settle(stage, probe);
            probe.note(format!(
                "rotated={}",
                probe.viewer().borrow().current_page()
            ));
        })
    });
    save(&image, "pdf-rotated-fit-page.png");
    assert_eq!(
        notes,
        [
            "actual=1.00",
            "in=1.10",
            "out=0.90",
            "width_again=true",
            "page=2",
            "last=5",
            "rotated=3"
        ],
        "{log:?}"
    );
    assert!(log.iter().any(|l| l == "PDF:ZOOM:110"), "{log:?}");
    assert!(
        log.iter().any(|l| l.starts_with("PDF:PAGE:DRAWN:4:")),
        "{log:?}"
    );
}

#[test]
fn a_locked_file_asks_for_its_password() {
    let file = TempFile::new("locked.pdf", LOCKED);
    let path = file.0.clone();
    let (_, log, notes) = watchdog(move || {
        drive(Theme::light(), move |stage, probe| {
            stage.emit(Msg::OpenChosen(path));
            probe.note(format!("before={}", probe.viewer().borrow().page_count()));
            stage.emit(Msg::Password(DialogAction::Accept("nope".into())));
            stage.emit(Msg::Password(DialogAction::Accept("lazyos".into())));
            settle(stage, probe);
            probe.note(format!("after={}", probe.viewer().borrow().page_count()));
        })
    });
    assert_eq!(notes, ["before=0", "after=6"]);
    assert_eq!(
        log.iter()
            .filter(|l| l.starts_with("PDF:OPEN:PASSWORD:"))
            .count(),
        2,
        "{log:?}"
    );
    assert!(
        log.iter().any(|l| l.starts_with("PDF:OPEN:PASS:")),
        "{log:?}"
    );
}

#[test]
fn a_broken_file_shows_why_instead_of_pages() {
    let file = TempFile::new("broken.pdf", b"%PDF-1.7\nthis is not a document\n");
    let path = file.0.clone();
    let missing = file.0.with_file_name("missing.pdf");
    let (image, log, notes) = watchdog(move || {
        drive(Theme::light(), move |stage, probe| {
            stage.emit(Msg::OpenChosen(missing));
            stage.emit(Msg::OpenChosen(path));
            probe.note(format!("error={:?}", probe.viewer().borrow().error));
        })
    });
    save(&image, "pdf-broken.png");
    assert!(
        log.iter()
            .any(|l| l.starts_with("PDF:OPEN:FAIL:") && l.contains("missing.pdf")),
        "{log:?}"
    );
    assert!(
        log.iter()
            .any(|l| l.ends_with("broken.pdf:not a PDF or damaged")),
        "{log:?}"
    );
    assert_eq!(
        notes,
        ["error=Some(\"broken.pdf could not be opened: not a PDF or damaged.\")"]
    );
    let (white, _) = ink(&image);
    assert!(white < 0.3, "no page is drawn: {white}");
}

#[test]
fn the_keyboard_maps_to_commands() {
    use xui_core::message::{Key, Modifiers};
    use xui_pdfview::shortcut;
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    };
    assert!(matches!(shortcut(Key::O, ctrl), Some(Msg::Open)));
    assert!(matches!(
        shortcut(Key::from_code(0xBB), ctrl),
        Some(Msg::ZoomStep(true))
    ));
    assert!(matches!(
        shortcut(Key::from_code(0xBD), ctrl),
        Some(Msg::ZoomStep(false))
    ));
    assert!(matches!(
        shortcut(Key::PAGE_DOWN, Modifiers::NONE),
        Some(Msg::ScrollBy(0, 9))
    ));
    assert!(matches!(
        shortcut(Key::END, Modifiers::NONE),
        Some(Msg::LastPage)
    ));
    assert!(matches!(
        shortcut(Key::N, Modifiers::NONE),
        Some(Msg::NextPage)
    ));
    assert!(matches!(
        shortcut(Key::PAGE_UP, ctrl),
        Some(Msg::PreviousPage)
    ));
    assert!(shortcut(Key::O, Modifiers::NONE).is_none());
}
