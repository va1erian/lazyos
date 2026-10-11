//! Shared by the window tests: an in-process Bluesky (sign-in, a paged
//! timeline, pictures, threads, likes, the repo writes), a platform that
//! records what the app logs, and the offscreen render helper.

#![allow(dead_code)]

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lazysky_core::{Platform, Roots, Secret, Settings};
use lazysky_ui::{LazySkyApp, Msg, Report};
use serde_json::Value;
use xui_canvas::snapshot::{render_with, Snapshot, Stage};
use xui_core::backend::Event;
use xui_core::message::Modifiers;
use xui_core::{Dip, Image, Theme};

mod bluesky;
#[allow(unused_imports)]
pub use bluesky::{bluesky, picture, post_view};

/// The signed-in account's DID: posts with `n % 13 == 5` are its own.
pub const ME: &str = "did:plc:me";

#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
}

impl Req {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

pub struct Mock {
    pub port: u16,
    seen: Arc<Mutex<Vec<Req>>>,
    /// While set, the repo writes answer 500.
    pub fail_writes: Arc<AtomicBool>,
}

impl Mock {
    pub fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn requests(&self) -> Vec<Req> {
        self.seen.lock().unwrap().clone()
    }

    /// The requests whose path contains `part`.
    pub fn to(&self, part: &str) -> Vec<Req> {
        self.requests()
            .into_iter()
            .filter(|r| r.path.contains(part))
            .collect()
    }
}

type Answer = dyn Fn(&Req) -> (u16, Vec<u8>) + Send + Sync;

/// A plain-HTTP server answering with `answer`.
pub fn serve(answer: impl Fn(&Req) -> (u16, Vec<u8>) + Send + Sync + 'static) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<Req>>> = Arc::default();
    let answer: Arc<Answer> = Arc::new(answer);
    let log = Arc::clone(&seen);
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (answer, log) = (Arc::clone(&answer), Arc::clone(&log));
            thread::spawn(move || {
                stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let mut words = line.split_whitespace();
                let method = words.next().unwrap_or("").to_string();
                let path = words.next().unwrap_or("").to_string();
                let mut length = 0usize;
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                    if let Some((n, v)) = line.split_once(':') {
                        if n.eq_ignore_ascii_case("content-length") {
                            length = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut body = vec![0; length];
                let _ = reader.read_exact(&mut body);
                let req = Req { method, path, body };
                log.lock().unwrap().push(req.clone());
                let (status, text) = answer(&req);
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    text.len()
                );
                let out = reader.get_mut();
                let _ = out.write_all(head.as_bytes());
                let _ = out.write_all(&text);
                let _ = out.flush();
            });
        }
    });
    Mock {
        port,
        seen,
        fail_writes: Arc::default(),
    }
}

/// What the app logged through `Platform::log`.
#[derive(Default)]
pub struct Logged(Mutex<Vec<String>>);

impl Logged {
    pub fn has(&self, needle: &str) -> bool {
        self.0.lock().unwrap().iter().any(|l| l.contains(needle))
    }

    pub fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

pub struct Recording(pub Arc<Logged>);

impl Platform for Recording {
    fn ca_roots(&self) -> Roots {
        Roots::Pem(Arc::default())
    }
    fn app_dir(&self) -> PathBuf {
        std::env::temp_dir()
    }
    fn pictures_dir(&self) -> PathBuf {
        std::env::temp_dir()
    }
    fn open_url(&self, url: &str) -> Result<(), String> {
        self.0 .0.lock().unwrap().push(format!("OPEN:{url}"));
        Ok(())
    }
    fn log(&self, line: &str) {
        self.0 .0.lock().unwrap().push(line.to_string());
    }
}

pub fn register_fonts() {
    let fonts: [&[u8]; 2] = [
        include_bytes!("../../../../../assets/fonts/DroidSans.ttf"),
        include_bytes!("../../../../../assets/fonts/DroidSans-Bold.ttf"),
    ];
    for font in fonts {
        xui_canvas::add_font(font.to_vec());
    }
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

/// Renders the window against `service`; `script` drives it.
pub fn render(
    theme: Theme,
    service: String,
    script: impl FnOnce(&Stage<'_, Msg>, &Heard) + 'static,
) -> Shot {
    let heard: Heard = Rc::default();
    let log: Arc<Logged> = Arc::default();
    let (sink, probe, platform) = (Rc::clone(&heard), Rc::clone(&heard), Arc::clone(&log));
    let image = render_with(
        Snapshot::new(Dip(800.0), Dip(560.0)).theme(theme),
        move |ui| {
            let settings = Settings {
                service,
                ..Settings::default()
            };
            LazySkyApp::build(ui, Arc::new(Recording(platform)), settings, move |msg| {
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

pub fn login(stage: &Stage<'_, Msg>) {
    stage.emit(Msg::Login {
        handle: "me.test".into(),
        password: Secret::new("abcd-efgh-ijkl-mnop"),
    });
}

/// Signs in and ticks until the first page of the timeline is shown.
pub fn sign_in_and_load(stage: &Stage<'_, Msg>, heard: &Heard) {
    login(stage);
    tick_until(stage, Duration::from_secs(30), || {
        heard.borrow().contains(&Report::FeedRows(30))
    });
}

pub fn wheel(stage: &Stage<'_, Msg>, notches: i16) {
    stage.inject(Event::MouseWheel {
        delta: notches,
        horizontal: false,
        x: 300,
        y: 300,
        modifiers: Modifiers::NONE,
    });
}

pub fn save(image: &Image, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

/// The row id the feed gives post `n` of the mock (no repost reason).
pub fn row_id(n: usize, base: &str) -> u64 {
    let post = lazysky_core::proto::Post::from_view(&post_view(n, base)).unwrap();
    lazysky_core::proto::FeedItem { post, reason: None }.row_id()
}
