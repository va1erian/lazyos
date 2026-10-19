//! Shared by the window tests: a scripted Fireworks (`server`), a platform
//! that records what the app logs and opens, and the offscreen render helper.

#![allow(dead_code)]

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lazyagent_core::{Endpoint, Platform, Roots};
use lazyagent_ui::{LazyAgentApp, Msg, Report};
use xui_canvas::snapshot::{render_with, Snapshot, Stage};
use xui_core::backend::Event;
use xui_core::message::Modifiers;
use xui_core::{Dip, Image, Theme};

mod server;
pub use server::*;

/// What the app logged and was asked to open.
#[derive(Default)]
pub struct Logged(pub Mutex<Vec<String>>);

struct Recording {
    logged: Arc<Logged>,
    dir: PathBuf,
    roots: Roots,
    endpoint: Endpoint,
}

impl Platform for Recording {
    fn ca_roots(&self) -> Roots {
        self.roots.clone()
    }
    fn app_dir(&self) -> PathBuf {
        self.dir.clone()
    }
    fn endpoint(&self) -> Endpoint {
        self.endpoint.clone()
    }
    fn open_url(&self, url: &str) -> Result<(), String> {
        self.logged.0.lock().unwrap().push(format!("OPEN:{url}"));
        Ok(())
    }
    fn log(&self, line: &str) {
        self.logged.0.lock().unwrap().push(line.to_string());
    }
}

pub fn temp_dir(name: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lazyagent-ui-{}-{name}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

pub fn register_fonts() {
    let fonts: [&[u8]; 2] = [
        include_bytes!("../../../../../assets/fonts/DroidSans.ttf"),
        include_bytes!("../../../../../assets/fonts/DroidSans-Bold.ttf"),
    ];
    for font in fonts {
        xui_canvas::add_font(font.to_vec());
    }
    // The code blocks' `monospace` family.
    xui_canvas::add_font(
        include_bytes!("../../../../../assets/fonts/liberation/LiberationMono-Regular.ttf")
            .to_vec(),
    );
    xui_canvas::set_default_family("Droid Sans");
}

/// Runs `test` on its own thread with the fonts, failing if it hangs.
pub fn watchdog<T: Send + 'static>(test: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        register_fonts();
        let _ = tx.send(test());
    });
    match rx.recv_timeout(Duration::from_secs(180)) {
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

pub struct Shot {
    pub image: Image,
    pub reports: Vec<Report>,
    pub log: Arc<Logged>,
}

pub type Heard = Rc<RefCell<Vec<Report>>>;

/// Renders the window against `server`; `script` drives it.
pub fn render(
    theme: Theme,
    pki: &Pki,
    server: &Server,
    dir: PathBuf,
    script: impl FnOnce(&Stage<'_, Msg>, &Heard) + 'static,
) -> Shot {
    let heard: Heard = Rc::default();
    let log: Arc<Logged> = Arc::default();
    let (sink, probe) = (Rc::clone(&heard), Rc::clone(&heard));
    let platform = Recording {
        logged: Arc::clone(&log),
        dir,
        roots: pki.roots(),
        endpoint: server.endpoint(),
    };
    let platform: Arc<dyn Platform> = Arc::new(platform);
    let image = render_with(
        Snapshot::new(Dip(960.0), Dip(640.0)).theme(theme),
        move |ui| {
            LazyAgentApp::build(ui, platform, move |msg| {
                if let Msg::Report(report) = msg {
                    sink.borrow_mut().push(report.clone());
                }
            })
        },
        move |stage| script(stage, &probe),
    )
    .expect("the headless render");
    let reports = heard.borrow().clone();
    Shot {
        image,
        reports,
        log,
    }
}

/// Ticks the window (as its timer would) until `done` or `limit` passes.
pub fn tick_until(stage: &Stage<'_, Msg>, limit: Duration, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() && started.elapsed() < limit {
        thread::sleep(Duration::from_millis(15));
        stage.emit(Msg::Tick);
    }
}

pub fn heard(heard: &Heard, wanted: impl Fn(&Report) -> bool) -> bool {
    heard.borrow().iter().any(wanted)
}

/// Accepts the consent screen and enters the key.
pub fn sign_in(stage: &Stage<'_, Msg>, heard_: &Heard) {
    tick_until(stage, Duration::from_secs(10), || {
        heard(heard_, |r| *r == Report::Ready)
    });
    stage.emit(Msg::Accept);
    tick_until(stage, Duration::from_millis(300), || false);
}

pub fn wheel(stage: &Stage<'_, Msg>, notches: i16) {
    stage.inject(Event::MouseWheel {
        delta: notches * 120,
        horizontal: false,
        x: 500,
        y: 300,
        modifiers: Modifiers::NONE,
    });
}

/// How many pixels differ strongly from the picture's most common colour:
/// text, rules and fills, not the background (a window whose rows are all
/// still skeletons has little).
pub fn ink(image: &Image) -> usize {
    let px = image.pixels();
    let mut counts = std::collections::HashMap::new();
    for p in px.as_chunks::<4>().0 {
        *counts.entry([p[0], p[1], p[2]]).or_insert(0usize) += 1;
    }
    let Some((bg, _)) = counts.into_iter().max_by_key(|(_, n)| *n) else {
        return 0;
    };
    let far = |a: u8, b: u8| i32::from(a).abs_diff(i32::from(b));
    px.as_chunks::<4>()
        .0
        .iter()
        .filter(|p| far(p[0], bg[0]) + far(p[1], bg[1]) + far(p[2], bg[2]) > 240)
        .count()
}

pub fn save(image: &Image, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
}
