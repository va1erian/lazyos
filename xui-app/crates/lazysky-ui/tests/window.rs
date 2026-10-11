//! The window built offscreen: the sign-in page, a sign-in against an
//! in-process HTTP server (timeline loaded through the tick drain and the
//! layout thread), scrolling, "Load more", a refused password, in both
//! themes. Snapshots land in `target/snapshots/lazysky-*.png` for a human to
//! look at.

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
use xui_canvas::snapshot::{render_with, Snapshot, Stage};
use xui_core::backend::Event;
use xui_core::message::Modifiers;
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

/// One post of the mock timeline: variety in length, links, pictures,
/// quotes and reposts, so the rows have different heights.
fn post(n: usize) -> String {
    let words = "the quick brown fox jumps over the lazy dog ";
    let text = words.repeat(1 + n % 5);
    let text = format!("Post number {n}: {}", text.trim());
    let mut record = format!(r#""record":{{"text":"{text}","createdAt":"2026-10-10T12:00:00Z"}}"#);
    if n.is_multiple_of(7) {
        // A link over "Post" (bytes 0..4).
        record = format!(
            r##""record":{{"text":"{text}","createdAt":"2026-10-10T12:00:00Z","facets":[{{"index":{{"byteStart":0,"byteEnd":4}},"features":[{{"$type":"app.bsky.richtext.facet#link","uri":"https://example.test/{n}"}}]}}]}}"##
        );
    }
    let embed = match n % 6 {
        1 => r#","embed":{"$type":"app.bsky.embed.images#view","images":[{"thumb":"https://cdn.test/t","fullsize":"https://cdn.test/f","aspectRatio":{"width":4,"height":3}}]}"#.to_string(),
        3 => r#","embed":{"$type":"app.bsky.embed.record#view","record":{"$type":"app.bsky.embed.record#viewRecord","uri":"at://did:plc:q/app.bsky.feed.post/1","cid":"c","author":{"did":"did:plc:q","handle":"q.test","displayName":"Quentin"},"value":{"text":"an older thought worth quoting"}}}"#.to_string(),
        _ => String::new(),
    };
    let reason = if n.is_multiple_of(11) {
        r#","reason":{"$type":"app.bsky.feed.defs#reasonRepost","by":{"did":"did:plc:r","handle":"r.test","displayName":"Rita"}}"#
    } else {
        ""
    };
    format!(
        r#"{{"post":{{"uri":"at://did:plc:a/app.bsky.feed.post/{n}","cid":"c{n}","author":{{"did":"did:plc:a","handle":"alice.test","displayName":"Alice"}},{record},"likeCount":{},"repostCount":{}{embed}}}{reason}}}"#,
        n % 40,
        n % 9
    )
}

/// A mock Bluesky: createSession and a 300-post timeline in pages of 30,
/// the cursor `page-<k>` naming the next page.
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
        let page: usize = path
            .split("cursor=page-")
            .nth(1)
            .and_then(|rest| rest.split('&').next()?.parse().ok())
            .unwrap_or(0);
        let posts: Vec<String> = (page * 30..page * 30 + 30).map(post).collect();
        let next = if page + 1 < 10 {
            format!(r#""cursor":"page-{}","#, page + 1)
        } else {
            String::new()
        };
        (200, format!(r#"{{{next}"feed":[{}]}}"#, posts.join(",")))
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
    script: impl FnOnce(&Stage<'_, Msg>, &Heard) + 'static,
) -> Shot {
    let heard: Heard = Rc::default();
    let (sink, probe) = (Rc::clone(&heard), Rc::clone(&heard));
    let image = render_with(
        Snapshot::new(Dip(800.0), Dip(560.0)).theme(theme),
        move |ui| {
            let settings = Settings { service };
            LazySkyApp::build(ui, Arc::new(Nowhere), settings, move |msg| {
                if let Msg::Report(report) = msg {
                    sink.borrow_mut().push(report.clone());
                }
            })
        },
        move |stage| script(stage, &probe),
    )
    .expect("the headless render");
    let reports = heard.borrow().clone();
    Shot { image, reports }
}

/// Ticks the window (as its timer would) until `done` or `limit` passes.
fn tick_until(stage: &Stage<'_, Msg>, limit: Duration, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() && started.elapsed() < limit {
        thread::sleep(Duration::from_millis(15));
        stage.emit(Msg::Tick);
    }
}

fn login(stage: &Stage<'_, Msg>) {
    stage.emit(Msg::Login {
        handle: "me.test".into(),
        password: Secret::new("abcd-efgh-ijkl-mnop"),
    });
}

fn wheel(stage: &Stage<'_, Msg>, notches: i16) {
    stage.inject(Event::MouseWheel {
        delta: notches,
        horizontal: false,
        x: 300,
        y: 300,
        modifiers: Modifiers::NONE,
    });
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
fn signing_in_loads_a_prepared_timeline_and_it_scrolls() {
    watchdog(|| {
        let port = bluesky();
        for (theme, tag) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
            let service = format!("http://127.0.0.1:{port}");
            let shot = render(theme, service, |stage, heard| {
                login(stage);
                tick_until(stage, Duration::from_secs(30), || {
                    heard.borrow().contains(&Report::FeedRows(30))
                });
                // Scroll down 40 notches; rows there are prepared by the
                // layout thread while the ticks run.
                wheel(stage, -40);
                tick_until(stage, Duration::from_millis(400), || false);
            });
            assert!(shot.reports.contains(&Report::FeedRows(30)), "{tag}");
            save(&shot.image, &format!("lazysky-feed-{tag}.png"));
        }
    });
}

#[test]
fn load_more_appends_the_next_page() {
    watchdog(|| {
        let port = bluesky();
        let shot = render(
            Theme::light(),
            format!("http://127.0.0.1:{port}"),
            |stage, heard| {
                login(stage);
                tick_until(stage, Duration::from_secs(30), || {
                    heard.borrow().contains(&Report::FeedRows(30))
                });
                stage.emit(Msg::LoadMore);
                tick_until(stage, Duration::from_secs(30), || {
                    heard.borrow().contains(&Report::FeedRows(60))
                });
            },
        );
        assert_eq!(shot.reports, [Report::FeedRows(30), Report::FeedRows(60)]);
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
            |stage, _| {
                stage.emit(Msg::Login {
                    handle: "me.test".into(),
                    password: Secret::new("wrong"),
                });
                // No report marks a failure: give the round trip a moment.
                tick_until(stage, Duration::from_millis(1500), || false);
            },
        );
        assert!(shot.reports.is_empty());
        save(&shot.image, "lazysky-login-refused-light.png");
    });
}
