//! Shared by the window tests: an in-process Bluesky (sign-in, a paged
//! timeline, pictures, threads, likes, the repo writes), a platform that
//! records what the app logs, and the offscreen render helper.

#![allow(dead_code)]

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lazysky_core::{Platform, Roots, Secret, Settings};
use lazysky_ui::{LazySkyApp, Msg, Report};
use serde_json::{json, Value};
use xui_canvas::snapshot::{render_with, Snapshot, Stage};
use xui_core::backend::Event;
use xui_core::message::Modifiers;
use xui_core::{Dip, Image, Theme};

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

/// A PNG with a colour gradient and a diagonal stripe, tinted by `n`.
pub fn picture(w: u32, h: u32, n: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(w, h, |x, y| {
        if (x + y) % 64 < 8 {
            image::Rgba([255, 255, 255, 255])
        } else {
            image::Rgba([
                (x * 255 / w) as u8,
                (y * 255 / h) as u8,
                (n.wrapping_mul(53) % 200) as u8,
                255,
            ])
        }
    });
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

/// The `postView` of post `n` (own when `n % 13 == 5`).
pub fn post_view(n: usize, base: &str) -> Value {
    let words = "the quick brown fox jumps over the lazy dog ";
    let text = format!("Post number {n}: {}", words.repeat(1 + n % 5).trim());
    let own = n % 13 == 5;
    let mut record = json!({"text": text, "createdAt": "2026-10-10T12:00:00Z"});
    if n.is_multiple_of(7) {
        record["facets"] = json!([{"index": {"byteStart": 0, "byteEnd": 4},
            "features": [{"$type": "app.bsky.richtext.facet#link",
                          "uri": format!("https://example.test/{n}")}]}]);
    }
    let mut view = json!({
        "uri": format!("at://{}/app.bsky.feed.post/{n}", if own { ME } else { "did:plc:a" }),
        "cid": format!("bafy{n}"),
        "author": {"did": if own { ME } else { "did:plc:a" },
                   "handle": if own { "me.test" } else { "alice.test" },
                   "displayName": if own { "Me" } else { "Alice" },
                   "avatar": format!("{base}/avatar.png")},
        "record": record, "likeCount": n % 40, "repostCount": n % 9,
        "replyCount": 0, "indexedAt": "2026-10-10T12:00:01Z"
    });
    match n % 6 {
        1 => {
            view["embed"] = json!({"$type": "app.bsky.embed.images#view", "images": [{
                "thumb": format!("{base}/img/{n}.png"),
                "fullsize": format!("{base}/img/{n}.png"),
                "aspectRatio": {"width": 4, "height": 3}}]});
        }
        3 => {
            view["embed"] = json!({"$type": "app.bsky.embed.record#view", "record": {
                "$type": "app.bsky.embed.record#viewRecord",
                "uri": "at://did:plc:q/app.bsky.feed.post/1", "cid": "c",
                "author": {"did": "did:plc:q", "handle": "q.test", "displayName": "Quentin"},
                "value": {"text": "an older thought worth quoting"}}});
        }
        _ => {}
    }
    view
}

/// The query value `key` of `path`, decoded.
fn query(path: &str, key: &str) -> Option<String> {
    let q = path.split_once('?')?.1;
    let raw = q
        .split('&')
        .find_map(|kv| kv.strip_prefix(&format!("{key}=")))?;
    let mut out = Vec::new();
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A mock Bluesky: one server is the login service and the PDS.
pub fn bluesky() -> Mock {
    let port = Arc::new(Mutex::new(0u16));
    let fail = Arc::new(AtomicBool::new(false));
    let (shared, fail_in) = (Arc::clone(&port), Arc::clone(&fail));
    let mut mock = serve(move |req| {
        let base = format!("http://127.0.0.1:{}", shared.lock().unwrap());
        let json = |status: u16, v: Value| (status, v.to_string().into_bytes());
        let path = req.path.as_str();
        if path.starts_with("/avatar.png") {
            return (200, picture(96, 96, 7));
        }
        if let Some(n) = path.strip_prefix("/img/") {
            let n: u32 = n.trim_end_matches(".png").parse().unwrap_or(0);
            return (200, picture(400, 300, n));
        }
        if path.contains("createSession") {
            return json(
                200,
                json!({"accessJwt": "a.b.c", "refreshJwt": "d.e.f", "did": ME,
                       "handle": "me.test",
                       "didDoc": {"service": [{"id": "#atproto_pds",
                                               "serviceEndpoint": base}]}}),
            );
        }
        if path.contains("createRecord") || path.contains("deleteRecord") {
            if fail_in.load(Ordering::SeqCst) {
                return json(
                    500,
                    json!({"error": "InternalServerError", "message": "the repo is down"}),
                );
            }
            if path.contains("deleteRecord") {
                return json(200, json!({}));
            }
            let body = req.json();
            let collection = body["collection"].as_str().unwrap_or("x");
            return json(
                200,
                json!({"uri": format!("at://{ME}/{collection}/3knew"), "cid": "bafynew"}),
            );
        }
        if path.contains("uploadBlob") {
            return json(
                200,
                json!({"blob": {"$type": "blob", "ref": {"$link": "bafyblob"},
                                "mimeType": "image/png", "size": req.body.len()}}),
            );
        }
        if path.contains("getPostThread") {
            let uri = query(path, "uri").unwrap_or_default();
            let n: usize = uri
                .rsplit('/')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let reply = |k: usize| {
                json!({"$type": "app.bsky.feed.defs#threadViewPost",
                       "post": post_view(1000 + k, &base), "replies": []})
            };
            return json(
                200,
                json!({"thread": {"$type": "app.bsky.feed.defs#threadViewPost",
                    "post": post_view(n, &base),
                    "replies": [reply(1), reply(2)]}}),
            );
        }
        if path.contains("getLikes") {
            return json(
                200,
                json!({"likes": [
                    {"actor": {"did": "did:plc:x", "handle": "x.test", "displayName": "Xavier"}},
                    {"actor": {"did": "did:plc:y", "handle": "y.test"}}]}),
            );
        }
        if path.contains("getTimeline") {
            let page: usize = query(path, "cursor")
                .and_then(|c| c.strip_prefix("page-").and_then(|n| n.parse().ok()))
                .unwrap_or(0);
            let feed: Vec<Value> = (page * 30..page * 30 + 30)
                .map(|n| {
                    let mut entry = json!({"post": post_view(n, &base)});
                    if n.is_multiple_of(11) {
                        entry["reason"] = json!({
                            "$type": "app.bsky.feed.defs#reasonRepost",
                            "by": {"did": "did:plc:r", "handle": "r.test", "displayName": "Rita"}});
                    }
                    entry
                })
                .collect();
            let mut answer = json!({"feed": feed});
            if page + 1 < 10 {
                answer["cursor"] = json!(format!("page-{}", page + 1));
            }
            return json(200, answer);
        }
        json(404, json!({"error": "NotFound", "message": "nothing here"}))
    });
    *port.lock().unwrap() = mock.port;
    mock.fail_writes = fail;
    mock
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
            let settings = Settings { service };
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
