//! The window built offscreen: the sign-in page, a sign-in against an
//! in-process HTTP server (timeline loaded through the tick drain), a refused
//! password, in both themes. Snapshots land in `target/snapshots/lazysky-*.png`
//! for a human to look at.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use lazysky_core::{Platform, Roots, Secret, Settings};
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
    let handle = thread::spawn(move || {
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

/// A plain-HTTP server: `answer(path)` is `(status, JSON body)`. Returns its
/// port.
fn serve(answer: impl Fn(&str) -> (u16, String) + Send + Sync + 'static) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let answer = Arc::new(answer);
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let answer = Arc::clone(&answer);
            thread::spawn(move || {
                stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
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
                let (status, text) = answer(&path);
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    text.len()
                );
                let out = reader.get_mut();
                let _ = out.write_all(head.as_bytes());
                let _ = out.write_all(text.as_bytes());
                let _ = out.flush();
            });
        }
    });
    port
}

/// A mock Bluesky: createSession and a 30-post timeline with a next cursor.
fn bluesky() -> u16 {
    let port = Arc::new(std::sync::Mutex::new(0u16));
    let shared = Arc::clone(&port);
    let actual = serve(move |path| {
        if path.contains("createSession") {
            let pds = format!("http://127.0.0.1:{}", shared.lock().unwrap());
            let session = format!(
                r##"{{"accessJwt":"a.b.c","refreshJwt":"d.e.f","did":"did:plc:me","handle":"me.test",
                    "didDoc":{{"service":[{{"id":"#atproto_pds","serviceEndpoint":"{pds}"}}]}}}}"##
            );
            return (200, session);
        }
        let posts: Vec<String> = (0..30)
            .map(|n| {
                format!(
                    r#"{{"post":{{"uri":"at://did:plc:a/app.bsky.feed.post/{n}","cid":"c{n}",
                    "author":{{"did":"did:plc:a","handle":"alice.test","displayName":"Alice"}},
                    "record":{{"text":"Post number {n}: the quick brown fox","createdAt":"2026-01-01T00:00:00Z"}}}}}}"#
                )
            })
            .collect();
        (
            200,
            format!(r#"{{"cursor":"more","feed":[{}]}}"#, posts.join(",")),
        )
    });
    *port.lock().unwrap() = actual;
    actual
}

struct Shot {
    image: Image,
    reports: Vec<Report>,
}

type Heard = Rc<RefCell<Vec<Report>>>;

fn render(
    theme: Theme,
    service: String,
    script: impl FnOnce(&dyn Fn(Msg), &Heard) + 'static,
) -> Shot {
    let heard: Heard = Rc::default();
    let (sink, probe) = (Rc::clone(&heard), Rc::clone(&heard));
    let image = render_with(
        Snapshot::new(Dip(640.0), Dip(420.0)).theme(theme),
        move |ui| {
            let settings = Settings { service };
            LazySkyApp::build(ui, Arc::new(Nowhere), settings, move |msg| {
                if let Msg::Report(report) = msg {
                    sink.borrow_mut().push(report.clone());
                }
            })
        },
        move |stage| script(&|msg| stage.emit(msg), &probe),
    )
    .expect("the headless render");
    let reports = heard.borrow().clone();
    Shot { image, reports }
}

/// Ticks the window (as its timer would) until `done` or `limit` passes.
fn tick_until(emit: &dyn Fn(Msg), limit: Duration, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() && started.elapsed() < limit {
        thread::sleep(Duration::from_millis(20));
        emit(Msg::Tick);
    }
}

fn save(image: &Image, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

#[test]
fn the_sign_in_page_renders_in_both_themes() {
    watchdog(|| {
        for (theme, tag) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
            let shot = render(theme, "http://127.0.0.1:1".into(), |_, _| {});
            assert!(shot.reports.is_empty());
            save(&shot.image, &format!("lazysky-login-{tag}.png"));
        }
    });
}

#[test]
fn signing_in_loads_the_timeline_through_the_tick_drain() {
    watchdog(|| {
        let port = bluesky();
        for (theme, tag) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
            let service = format!("http://127.0.0.1:{port}");
            let shot = render(theme, service, |emit, heard| {
                emit(Msg::Login {
                    handle: "me.test".into(),
                    password: Secret::new("abcd-efgh-ijkl-mnop"),
                });
                tick_until(emit, Duration::from_secs(20), || {
                    heard.borrow().contains(&Report::FeedRows(30))
                });
            });
            assert!(
                shot.reports.contains(&Report::FeedRows(30)),
                "{tag}: {:?}",
                shot.reports
            );
            save(&shot.image, &format!("lazysky-home-{tag}.png"));
        }
    });
}

#[test]
fn a_refused_password_is_shown_on_the_sign_in_page() {
    watchdog(|| {
        let port = serve(|_| {
            (
                401,
                r#"{"error":"AuthenticationRequired","message":"Invalid identifier or password"}"#
                    .to_string(),
            )
        });
        let shot = render(
            Theme::light(),
            format!("http://127.0.0.1:{port}"),
            |emit, _| {
                emit(Msg::Login {
                    handle: "me.test".into(),
                    password: Secret::new("wrong"),
                });
                // No report marks a failure: give the round trip a moment.
                tick_until(emit, Duration::from_millis(1500), || false);
            },
        );
        assert!(shot.reports.is_empty());
        save(&shot.image, "lazysky-login-refused-light.png");
    });
}
