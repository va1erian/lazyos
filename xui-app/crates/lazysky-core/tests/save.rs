//! Saving pictures: one to a chosen path (byte-for-byte, through `.part`)
//! and an account's whole gallery (own posts only, resumable, cancellable).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use image::{ImageFormat, Rgba, RgbaImage};
use lazysky_core::{Core, Event, Events, Platform, Request, Roots, Secret, Settings};
use serde_json::{json, Value};

fn png(seed: u8) -> Vec<u8> {
    let img = RgbaImage::from_fn(16, 12, |x, y| Rgba([seed, x as u8 * 8, y as u8 * 8, 255]));
    let mut out = Vec::new();
    img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .unwrap();
    out
}

type Answer = dyn Fn(&str, &[u8]) -> (u16, Vec<u8>) + Send + Sync;

struct Server {
    port: u16,
    hits: Arc<Mutex<Vec<String>>>,
}

fn serve(answer: impl Fn(&str, &[u8]) -> (u16, Vec<u8>) + Send + Sync + 'static) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits: Arc<Mutex<Vec<String>>> = Arc::default();
    let answer: Arc<Answer> = Arc::new(answer);
    let log = Arc::clone(&hits);
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (answer, log) = (Arc::clone(&answer), Arc::clone(&log));
            thread::spawn(move || {
                stream.set_read_timeout(Some(Duration::from_secs(20))).ok();
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
                log.lock().unwrap().push(path.clone());
                let (status, out) = answer(&path, &body);
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    out.len()
                );
                let stream = reader.get_mut();
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&out);
                let _ = stream.flush();
            });
        }
    });
    Server { port, hits }
}

#[derive(Default)]
struct Log(Mutex<Vec<String>>);

struct Testing(Arc<Log>);

impl Platform for Testing {
    fn ca_roots(&self) -> Roots {
        Roots::Pem(Arc::default())
    }
    fn app_dir(&self) -> PathBuf {
        std::env::temp_dir()
    }
    fn pictures_dir(&self) -> PathBuf {
        std::env::temp_dir()
    }
    fn open_url(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn log(&self, line: &str) {
        self.0 .0.lock().unwrap().push(line.to_string());
    }
}

fn start(server: &Server) -> (Core, Events, Arc<Log>) {
    let log: Arc<Log> = Arc::default();
    let settings = Settings {
        service: format!("http://127.0.0.1:{}", server.port),
        ..Settings::default()
    };
    let (core, events) = Core::start(Arc::new(Testing(log.clone())), settings);
    (core, events, log)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lazysky-save-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn wait<T>(events: &Events, mut pick: impl FnMut(Event) -> Option<T>) -> T {
    loop {
        match events.wait(Duration::from_secs(20)) {
            Some(event) => {
                if let Some(found) = pick(event) {
                    return found;
                }
            }
            None => panic!("the event never came"),
        }
    }
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn a_picture_is_saved_byte_for_byte_through_a_part_file() {
    let original = png(9);
    let served = original.clone();
    let server = serve(move |path, _| match path {
        "/p.png" => (200, served.clone()),
        _ => (404, Vec::new()),
    });
    let (core, events, log) = start(&server);
    let dir = scratch("one");
    let target = dir.join("nested").join("picture.png");
    core.send(Request::SaveImage {
        seq: 5,
        url: format!("http://127.0.0.1:{}/p.png", server.port),
        path: target.clone(),
    });
    let bytes = wait(&events, |e| match e {
        Event::Saved { seq: 5, bytes, .. } => Some(bytes),
        _ => None,
    });
    assert_eq!(bytes as usize, original.len());
    assert_eq!(std::fs::read(&target).unwrap(), original);
    assert_eq!(
        files_in(target.parent().unwrap()),
        ["picture.png"],
        "no .part left"
    );
    let lines = log.0.lock().unwrap().join("\n");
    assert!(
        lines.contains(&format!(
            "SKY:SAVE:DONE:{}:{}",
            target.display(),
            original.len()
        )),
        "{lines}"
    );
}

#[test]
fn a_failed_save_leaves_nothing_behind() {
    let server = serve(|_, _| (404, b"gone".to_vec()));
    let (core, events, log) = start(&server);
    let dir = scratch("fail");
    let target = dir.join("never.png");
    core.send(Request::SaveImage {
        seq: 6,
        url: format!("http://127.0.0.1:{}/missing.png", server.port),
        path: target.clone(),
    });
    let reason = wait(&events, |e| match e {
        Event::Failed { seq: 6, reason, .. } => Some(reason),
        _ => None,
    });
    assert!(reason.contains("404"), "{reason}");
    assert!(files_in(&dir).is_empty());
    assert!(log
        .0
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.starts_with("SKY:SAVE:FAIL:")));
}

/// A PDS whose `getAuthorFeed` has two pages of Alice's media posts:
/// two own posts with 2 and 1 pictures, a repost with a picture, a reply
/// with a picture and another account's post.
fn gallery(slow: Option<Arc<AtomicBool>>) -> (Server, HashMap<String, Vec<u8>>) {
    let mut pictures = HashMap::new();
    let mut page = |cids: &[&str], reason: bool, reply: bool, author: &str| -> Value {
        let images: Vec<Value> = cids
            .iter()
            .map(|cid| {
                pictures.insert(format!("/img/{cid}@jpeg"), png(cid.len() as u8));
                json!({"thumb": format!("http://IMG/thumb/{cid}@jpeg"),
                       "fullsize": format!("http://IMG/img/{cid}@jpeg"), "alt": ""})
            })
            .collect();
        let mut record = json!({"text": "t", "createdAt": "2026-10-10T12:00:00.000Z"});
        if reply {
            record["reply"] = json!({
                "root": {"uri": "at://did:plc:z/app.bsky.feed.post/1", "cid": "c"},
                "parent": {"uri": "at://did:plc:z/app.bsky.feed.post/1", "cid": "c"}});
        }
        let mut entry = json!({"post": {
            "uri": format!("at://{author}/app.bsky.feed.post/{}", cids[0]),
            "cid": "c", "author": {"did": author, "handle": if author == "did:plc:alice" { "alice.test" } else { "other.test" }},
            "record": record,
            "embed": {"$type": "app.bsky.embed.images#view", "images": images}}});
        if reason {
            entry["reason"] = json!({"$type": "app.bsky.feed.defs#reasonRepost",
                "by": {"did": "did:plc:alice", "handle": "alice.test"}});
        }
        entry
    };
    let first = json!({"cursor": "p2", "feed": [
        page(&["aaaaaaaaaaaa1", "bbbbbbbbbbbb2"], false, false, "did:plc:alice"),
        page(&["rrrrrrrrrrrr3"], true, false, "did:plc:other"),
    ]});
    let second = json!({"feed": [
        page(&["cccccccccccc4"], false, false, "did:plc:alice"),
        page(&["dddddddddddd5"], false, true, "did:plc:alice"),
        page(&["eeeeeeeeeeee6"], false, false, "did:plc:other"),
    ]});
    let served = pictures.clone();
    let port = Arc::new(Mutex::new(0u16));
    let shared = Arc::clone(&port);
    let server = serve(move |path, _| {
        if path.contains("createSession") {
            return (
                200,
                json!({"accessJwt": "a.b.c", "refreshJwt": "d.e.f",
                       "did": "did:plc:alice", "handle": "alice.test"})
                .to_string()
                .into_bytes(),
            );
        }
        if path.contains("getAuthorFeed") {
            let v = if path.contains("cursor=p2") {
                &second
            } else {
                &first
            };
            let base = format!("http://127.0.0.1:{}", shared.lock().unwrap());
            return (200, v.to_string().replace("http://IMG", &base).into_bytes());
        }
        if let Some(bytes) = served.get(path) {
            if let Some(flag) = &slow {
                // Hold every picture until the test lets go.
                while !flag.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(10));
                }
            }
            return (200, bytes.clone());
        }
        (404, Vec::new())
    });
    *port.lock().unwrap() = server.port;
    (server, pictures)
}

#[test]
fn bulk_download_saves_only_the_accounts_own_non_reply_pictures_and_resumes() {
    let (server, pictures) = gallery(None);
    let (core, events, log) = start(&server);
    login(&core, &events);
    let dir = scratch("bulk");
    core.send(Request::BulkDownload {
        seq: 9,
        actor: "alice.test".into(),
        dir: dir.clone(),
    });
    let done = wait(&events, |e| match e {
        Event::BulkDone {
            saved,
            skipped,
            errors,
            cancelled,
            ..
        } => Some((saved, skipped, errors, cancelled)),
        _ => None,
    });
    // aaaa..1, bbbb..2 and cccc..4 are Alice's own non-reply pictures.
    assert_eq!(done, (3, 0, 0, false), "{done:?}");
    let names = files_in(&dir);
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(names
        .iter()
        .all(|n| n.starts_with("img2026-10-10T12-00-00.000Z_")));
    assert!(names.iter().all(|n| n.ends_with(".jpg")));
    let first = std::fs::read(dir.join(&names[0])).unwrap();
    assert!(pictures.values().any(|p| *p == first));
    assert!(log
        .0
        .lock()
        .unwrap()
        .iter()
        .any(|l| l == "SKY:BULK:DONE:3:0"));

    // Again: everything is already there and left alone.
    core.send(Request::BulkDownload {
        seq: 10,
        actor: "alice.test".into(),
        dir: dir.clone(),
    });
    let again = wait(&events, |e| match e {
        Event::BulkDone {
            seq: 10,
            saved,
            skipped,
            ..
        } => Some((saved, skipped)),
        _ => None,
    });
    assert_eq!(again, (0, 3));
    assert!(!server.hits.lock().unwrap().is_empty());
}

#[test]
fn cancelling_a_bulk_download_stops_it_and_says_so() {
    let release = Arc::new(AtomicBool::new(false));
    let (server, _) = gallery(Some(Arc::clone(&release)));
    let (core, events, _log) = start(&server);
    login(&core, &events);
    let dir = scratch("cancel");
    core.send(Request::BulkDownload {
        seq: 11,
        actor: "alice.test".into(),
        dir: dir.clone(),
    });
    // Wait for the scan to find pictures, then cancel and release the held
    // fetches.
    wait(&events, |e| match e {
        Event::BulkProgress { found, .. } if found >= 1 => Some(()),
        _ => None,
    });
    core.cancel_bulk();
    release.store(true, Ordering::SeqCst);
    let cancelled = wait(&events, |e| match e {
        Event::BulkDone { cancelled, .. } => Some(cancelled),
        _ => None,
    });
    assert!(cancelled);
}

fn login(core: &Core, events: &Events) {
    core.send(Request::Login {
        seq: 1,
        handle: "alice.test".into(),
        password: Secret::new("pw"),
    });
    wait(events, |e| match e {
        Event::LoggedIn { .. } => Some(()),
        _ => None,
    });
}
