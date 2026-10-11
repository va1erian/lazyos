//! The spike window built offscreen: text shaped on a worker thread arrives
//! and is drawn by `Canvas::draw_layout`; a snapshot per theme
//! (`target/snapshots/lazysky-spike-*.png`) is left for a human to look at.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use lazysky_core::{Platform, Roots};
use lazysky_ui::{LazySkyApp, Msg, Report};
use xui_canvas::snapshot::{render_with, Snapshot};
use xui_core::{Dip, Image, Theme};

struct Nowhere;

impl Platform for Nowhere {
    fn ca_roots(&self) -> Roots {
        Roots::Pem(Arc::default())
    }
    fn app_dir(&self) -> PathBuf {
        std::env::temp_dir()
    }
    fn pictures_dir(&self) -> PathBuf {
        std::env::temp_dir()
    }
    fn open_url(&self, _url: &str) -> Result<(), String> {
        Err("no browser in a test".into())
    }
    fn log(&self, _line: &str) {}
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

/// Run `test` on its own thread with the fonts, failing if it hangs.
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
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("the window test hung"),
        Err(mpsc::RecvTimeoutError::Disconnected) => match handle.join() {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(()) => panic!("the window test ended without a result"),
        },
    }
}

fn render(theme: Theme) -> (Image, Vec<Report>) {
    let heard: Rc<RefCell<Vec<Report>>> = Rc::default();
    let sink = Rc::clone(&heard);
    let image = render_with(
        Snapshot::new(Dip(640.0), Dip(360.0)).theme(theme),
        move |ui| {
            LazySkyApp::build(ui, Arc::new(Nowhere), move |msg| {
                if let Msg::Report(report) = msg {
                    sink.borrow_mut().push(report.clone());
                }
            })
        },
        |stage| {
            // The offscreen backend has no timers: wait for the worker, then
            // deliver the tick the timer would have.
            std::thread::sleep(Duration::from_millis(500));
            stage.emit(Msg::Poll);
        },
    )
    .expect("the headless render");
    let heard = heard.borrow().clone();
    (image, heard)
}

fn save(image: &Image, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

#[test]
fn text_shaped_on_a_worker_thread_is_drawn_in_both_themes() {
    watchdog(|| {
        for (theme, tag) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
            let (image, heard) = render(theme);
            assert!(
                matches!(heard.as_slice(), [Report::ShapedOffThread { lines_height }] if *lines_height > 0),
                "{tag}: {heard:?}"
            );
            save(&image, &format!("lazysky-spike-{tag}.png"));
        }
    });
}
