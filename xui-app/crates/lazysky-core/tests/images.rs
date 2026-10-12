//! The image threads against an in-process HTTP server: cover crops at the
//! exact size, circle avatars, refusals (bomb, junk, 404), and that a new
//! wanted list drops what waited and is no longer wanted (R6).

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Cursor, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use image::{ImageFormat, Rgba, RgbaImage};
use lazysky_core::{Core, Event, ImageKey, Platform, Roots, Settings, Shape};

fn png(w: u32, h: u32, colour: [u8; 4]) -> Vec<u8> {
    let img = RgbaImage::from_pixel(w, h, Rgba(colour));
    let mut out = Vec::new();
    img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .unwrap();
    out
}

/// A real PNG whose header claims 70,000 x 70,000: what a decompression
/// bomb's first bytes look like.
fn bomb() -> Vec<u8> {
    fn crc(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    let mut real = png(1, 1, [0, 0, 0, 255]);
    // IHDR: length at 8..12, type and data at 12..29, CRC at 29..33.
    real[16..20].copy_from_slice(&70_000u32.to_be_bytes());
    real[20..24].copy_from_slice(&70_000u32.to_be_bytes());
    let sum = crc(&real[12..29]);
    real[29..33].copy_from_slice(&sum.to_be_bytes());
    real
}

struct Server {
    port: u16,
    hits: Arc<Mutex<Vec<String>>>,
}

type Answer = dyn Fn(&str) -> (u16, Vec<u8>) + Send + Sync;

fn serve(answer: impl Fn(&str) -> (u16, Vec<u8>) + Send + Sync + 'static) -> Server {
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
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                }
                log.lock().unwrap().push(path.clone());
                let (status, body) = answer(&path);
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let out = reader.get_mut();
                let _ = out.write_all(head.as_bytes());
                let _ = out.write_all(&body);
                let _ = out.flush();
            });
        }
    });
    Server { port, hits }
}

struct Plain;

impl Platform for Plain {
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
    fn log(&self, _: &str) {}
}

fn key(server: &Server, path: &str, w: u32, h: u32, shape: Shape) -> ImageKey {
    ImageKey {
        url: format!("http://127.0.0.1:{}{path}", server.port),
        width: w,
        height: h,
        shape,
    }
}

fn image_events(
    events: &lazysky_core::Events,
    n: usize,
) -> Vec<(ImageKey, Result<lazysky_core::Bitmap, String>)> {
    let mut out = Vec::new();
    while out.len() < n {
        match events.wait(Duration::from_secs(20)) {
            Some(Event::Image { key, result }) => out.push((key, result)),
            Some(_) => {}
            None => panic!("only {} of {n} pictures arrived", out.len()),
        }
    }
    out
}

#[test]
fn pictures_arrive_at_the_exact_size_and_avatars_are_round() {
    let server = serve(|path| match path {
        "/wide.png" => (200, png(400, 100, [200, 30, 30, 255])),
        _ => (404, Vec::new()),
    });
    let (core, events) = Core::start(Arc::new(Plain), Settings::default());
    let cover = key(&server, "/wide.png", 60, 40, Shape::Cover);
    let avatar = key(&server, "/wide.png", 48, 48, Shape::Circle);
    core.want_images(vec![cover.clone(), avatar.clone()]);
    for (k, result) in image_events(&events, 2) {
        let bitmap = result.unwrap();
        assert_eq!((bitmap.width, bitmap.height), (k.width, k.height));
        let alpha = |x: u32, y: u32| bitmap.rgba[((y * bitmap.width + x) * 4 + 3) as usize];
        if k.shape == Shape::Circle {
            assert_eq!(alpha(0, 0), 0, "the corner is clear");
            assert_eq!(alpha(24, 24), 255, "the middle is solid");
        } else {
            assert_eq!(alpha(0, 0), 255);
            assert_eq!(&bitmap.rgba[..3], [200, 30, 30]);
        }
    }
}

#[test]
fn a_bomb_junk_and_a_missing_picture_are_reported_not_decoded() {
    let server = serve(|path| match path {
        "/bomb.png" => (200, bomb()),
        "/junk" => (200, b"<html>not a picture</html>".to_vec()),
        _ => (404, Vec::new()),
    });
    let (core, events) = Core::start(Arc::new(Plain), Settings::default());
    core.want_images(vec![
        key(&server, "/bomb.png", 10, 10, Shape::Cover),
        key(&server, "/junk", 10, 10, Shape::Cover),
        key(&server, "/gone.png", 10, 10, Shape::Cover),
    ]);
    let mut reasons = Vec::new();
    for (k, result) in image_events(&events, 3) {
        reasons.push((
            k.url.rsplit('/').next().unwrap().to_string(),
            result.unwrap_err(),
        ));
    }
    reasons.sort();
    assert!(
        reasons[0].1.contains("larger than this client decodes"),
        "{reasons:?}"
    );
    assert!(reasons[1].1.contains("answered 404"), "{reasons:?}");
    assert!(reasons[2].1.contains("not a picture"), "{reasons:?}");
}

#[test]
fn a_new_list_drops_what_waited_and_is_no_longer_wanted() {
    // The four image threads are held by gated pictures; the rest wait.
    let (release_tx, release_rx): (Sender<()>, Receiver<()>) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let gate = Arc::clone(&release_rx);
    let server = serve(move |path| {
        if path.starts_with("/gate") {
            let _ = gate.lock().unwrap().recv_timeout(Duration::from_secs(20));
        }
        (200, png(8, 8, [1, 2, 3, 255]))
    });
    let (core, events) = Core::start(Arc::new(Plain), Settings::default());
    let k = |p: &str| key(&server, p, 8, 8, Shape::Cover);
    core.want_images(vec![k("/gate0"), k("/gate1"), k("/gate2"), k("/gate3")]);
    // Wait until all four are in flight at the server.
    for _ in 0..200 {
        if server.hits.lock().unwrap().len() >= 4 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(server.hits.lock().unwrap().len(), 4);
    core.want_images(vec![k("/old1"), k("/old2"), k("/keep")]);
    // The user scrolled: only /keep is still wanted, and /new is nearer.
    core.want_images(vec![k("/new"), k("/keep")]);
    for _ in 0..4 {
        release_tx.send(()).unwrap();
    }
    let got = image_events(&events, 6);
    let paths: HashSet<String> = got
        .iter()
        .map(|(k, r)| {
            assert!(r.is_ok());
            k.url.rsplit_once('/').unwrap().1.to_string()
        })
        .collect();
    let expect: HashSet<String> = ["gate0", "gate1", "gate2", "gate3", "new", "keep"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(paths, expect);
    let fetched = server.hits.lock().unwrap().clone();
    assert!(
        !fetched.iter().any(|p| p.starts_with("/old")),
        "dropped before starting: {fetched:?}"
    );
}
