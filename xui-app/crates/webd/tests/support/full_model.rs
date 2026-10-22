//! What the traffic fuzz (`full_fuzz.rs`) knows about the views it made, the engine's news and the
//! clients' calls it generates, and the checks of every event a client receives against that
//! knowledge (the model of the slots: attached, busy, numbered frames).

#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};

use fuzzkit::Rng;
use messenger_generated::os_lazy_webd_light_v1 as light;
use messenger_generated::os_lazy_webd_view_v1 as wire;
use xui_webd::engine::EngineEvents;
use xui_webd::events::PageNow;
use xui_webd::fabric::errno::*;
use xui_webd::ident::Origin;
use xui_webd::limits::{SLOTS_PER_VIEW, SLOTS_WITH_BANDS};

use super::full::{decode_full_event, FEv, Fx};
use super::{decode_event, Ev};

/// What the test knows about one view it made.
pub struct Known {
    pub client: usize,
    pub view: u64,
    pub channel: u64,
    pub width: u32,
    pub height: u32,
    pub full: bool,
    pub deltas: bool,
    pub attached: [bool; SLOTS_WITH_BANDS],
    pub busy: [bool; SLOTS_WITH_BANDS],
    pub last_seq: u64,
    pub lost: bool,
}

impl Known {
    pub fn new(
        client: usize,
        view: u64,
        channel: u64,
        size: (u32, u32),
        full: bool,
        deltas: bool,
    ) -> Self {
        Known {
            client,
            view,
            channel,
            width: size.0,
            height: size.1,
            full,
            deltas,
            attached: [false; SLOTS_WITH_BANDS],
            busy: [false; SLOTS_WITH_BANDS],
            last_seq: 0,
            lost: false,
        }
    }

    /// Every slot is gone (a resize, or the view was lost).
    pub fn detach_all(&mut self) {
        (self.attached, self.busy) = ([false; SLOTS_WITH_BANDS], [false; SLOTS_WITH_BANDS]);
    }
}

/// How many events of each kind the fuzz saw clients receive, over all seeds: a fuzz that never
/// produced a delta or a band would pass without testing them.
pub static SEEN: [AtomicUsize; 6] = [const { AtomicUsize::new(0) }; 6];
pub const WHOLE: usize = 0;
pub const DELTA: usize = 1;
pub const BAND: usize = 2;
pub const ASKED: usize = 3;
pub const DIALOG: usize = 4;
pub const DOWNLOAD: usize = 5;
pub const SEEN_NAMES: [&str; 6] = ["whole", "delta", "band", "question", "dialog", "download"];

fn saw(kind: usize) {
    SEEN[kind].fetch_add(1, Ordering::Relaxed);
}

const URLS: [&str; 10] = [
    "about:blank",
    "http://10.0.2.2:18081/x",
    "https://example.com/a",
    "http://evil.test/",
    "file:///etc/passwd",
    "mailto:a@b",
    "javascript:alert(1)",
    "data:text/html,x",
    "",
    "http://[::1]:80/",
];

pub fn url(rng: &mut Rng) -> &'static str {
    URLS[rng.below(URLS.len() as u64) as usize]
}

/// The engine says something about full view `view` of `size` (`id` is a fresh number).
pub fn engine_news(fx: &mut Fx, rng: &mut Rng, view: u64, size: (i32, i32), id: u64) {
    let (w, h) = size;
    let wrong = if rng.one_in(10) { 1 } else { 0 };
    let shade = rng.byte();
    let picture = |w: i32| vec![shade; (w * h * 4) as usize];
    match rng.below(26) {
        0..=3 => fx.queue.frame(view, &picture(w + wrong), w + wrong, h),
        4 => fx.queue.scroll(
            view,
            rng.below(50) as i32,
            rng.below(50) as i32,
            rng.one_in(2),
        ),
        5..=8 => {
            let shift = rng
                .one_in(2)
                .then(|| [0, 0, w, h, 0, -(rng.below(3) as i32)]);
            // Mostly rectangles inside the picture; now and then one that is not.
            let rects: Vec<[i32; 4]> = (0..rng.below(3))
                .map(|_| {
                    let (x, y) = (rng.below(w as u64) as i32, rng.below(h as u64) as i32);
                    let slack = if rng.one_in(8) { 2 } else { 1 };
                    let rw = rng.below((w - x) as u64 + slack) as i32;
                    let rh = rng.below((h - y) as u64 + slack) as i32;
                    [x, y, rw, rh]
                })
                .collect();
            fx.queue
                .frame_delta(view, &picture(w + wrong), w + wrong, h, shift, &rects);
        }
        9 | 10 => {
            let (bw, bh) = (w + wrong, rng.range(1, 4) as i32);
            let rows = vec![shade; (bw * bh * 4) as usize];
            fx.queue.band(view, 0, rng.below(500) as i32, bw, bh, &rows);
        }
        11 => {
            let page = PageNow {
                title: "t".into(),
                progress: 100,
                finished: 1,
                ..PageNow::default()
            };
            fx.queue.page_state(view, page);
        }
        12 => fx.queue.hover(view, url(rng)),
        13 => fx.queue.cursor(view, rng.below(10) as i32 - 1),
        14 => {
            fx.queue.responsive(view, rng.one_in(2));
            fx.queue.blocked(view, rng.below(9) as i32 - 1);
        }
        15 => fx.queue.dialog(view, id, rng.below(4) as i32, "m", "d"),
        16 => fx.queue.new_window(view, url(rng), rng.one_in(2)),
        17 | 18 => {
            let kind = rng.below(7) as i32;
            let main_frame = !rng.one_in(4);
            fx.queue
                .navigate(view, id, url(rng), kind, rng.one_in(2), main_frame);
        }
        19 => {
            let site = "https://example.com";
            let earlier = id.saturating_sub(rng.below(5));
            match rng.below(5) {
                0 => fx.queue.notify_show(view, id, site, "t", "b"),
                1 => fx.queue.notify_permission_request(view, site),
                2 => fx.queue.notify_cancel(view, earlier),
                3 => fx.queue.notify_gone(view, earlier),
                _ => fx.queue.notify_permission_gone(view, site),
            }
        }
        20..=22 => download(fx, rng, view, id),
        23 => fx
            .queue
            .blocking_list(view, "easy", rng.below(6) as i32 - 1),
        24 => fx.queue.link_clicked(view, url(rng)),
        _ => fx.queue.clipboard_write(view, "copied"),
    }
}

fn download(fx: &mut Fx, rng: &mut Rng, view: u64, id: u64) {
    let names = ["a.txt", "../x", "", "a\u{202e}b.exe"];
    let name = names[rng.below(names.len() as u64) as usize];
    let earlier = id.saturating_sub(rng.below(3));
    match rng.below(4) {
        0 | 1 => {
            let total = rng.below(9) as i64 - 1;
            if let Some(part) = fx.queue.download_start(view, id, url(rng), name, total) {
                if rng.one_in(2) {
                    let _ = std::fs::write(part, b"x");
                }
            }
        }
        2 => fx.queue.download_progress(view, earlier, 1, 2),
        _ if rng.one_in(2) => fx.queue.download_done(view, earlier),
        _ => fx.queue.download_fail(view, earlier, "r"),
    }
}

/// The engine says something about light view `view`.
pub fn light_news(fx: &mut Fx, rng: &mut Rng, view: u64, size: (i32, i32)) {
    let (w, h) = size;
    match rng.below(5) {
        0 | 1 => fx.rig.paint(view, w + rng.below(2) as i32, h, rng.byte()),
        2 => fx
            .queue
            .state(view, "t", rng.one_in(2), rng.below(4) as i32, ""),
        3 => fx.queue.scroll(view, 1, 2, rng.one_in(2)),
        _ => fx.queue.link_clicked(view, url(rng)),
    }
}

/// A call on full view `view` of `client`, sometimes from the other client: only its owner is obeyed,
/// and every refusal is one of the expected errnos.
pub fn client_call(
    fx: &mut Fx,
    rng: &mut Rng,
    who: &[Origin; 2],
    client: usize,
    view: u64,
    ids: u64,
) {
    let caller = if rng.one_in(5) { 1 - client } else { client };
    let from = who[caller];
    let request = 1 + rng.below(ids + 1);
    let result = match rng.below(17) {
        0 => fx.load_url(from, view, url(rng)),
        1 => fx.reload(from, view, rng.one_in(2)),
        2 => fx.stop(from, view),
        3 => fx.download(from, view, url(rng)),
        4 => fx.download_cancel(from, view, request),
        5 => fx.dialog_reply(from, view, request, rng.one_in(2), "v"),
        6 | 7 => fx.nav_answer(from, view, request, rng.one_in(2)),
        8 => fx.notify_permission(from, view, "https://example.com", rng.below(4) as u32),
        9 => fx.set_blocking(from, view, rng.one_in(2), &["example.com", "a.test"]),
        10 => fx.blocking_compile(from, view, "easy", b"[]"),
        11 => fx.blocking_load(from, view, "easy"),
        12 => fx.mouse(from, view, rng.below(3) as u32),
        13 => fx.key(from, view, rng.one_in(2)),
        14 => fx.recover(from, view, rng.one_in(2)),
        15 => fx.notify_forget(from, view, "https://example.com"),
        _ => fx.load_page(from, view, b"<p>x</p>", url(rng)),
    };
    match result {
        Ok(_) => assert_eq!(caller, client, "someone else's view obeyed"),
        Err(code) => {
            let allowed: &[i64] = if caller == client {
                &[ENOENT, ENODEV, EACCES, EINVAL, ESRCH, E2BIG]
            } else {
                &[ENOENT, EINVAL]
            };
            assert!(allowed.contains(&code), "errno {code}");
        }
    }
}

/// Read every channel's events and check them against what the client knows.
pub fn observe(fx: &mut Fx, known: &mut [Known]) {
    for k in known.iter_mut() {
        let tagged = fx.fabric.take_tagged(k.channel);
        let interface = if k.full {
            wire::INTERFACE_ID
        } else {
            light::INTERFACE_ID
        };
        for (tag, method, body) in tagged {
            assert_eq!(tag, interface, "an event of the other kind of view");
            if k.full {
                full_event(k, decode_full_event(method, &body));
            } else {
                light_event(k, decode_event(method, &body));
            }
        }
    }
}

/// A frame the client was told about: in range, attached, free, numbered one by one.
fn took_frame(k: &mut Known, slot: u32, seq: u64, size: (u32, u32)) {
    let index = slot as usize;
    assert!(!k.lost, "a picture of a lost view");
    assert!(index < SLOTS_PER_VIEW, "a frame outside the frame slots");
    assert!(
        k.attached[index] && !k.busy[index],
        "a frame in a slot that was not free"
    );
    assert_eq!(seq, k.last_seq + 1, "frames are numbered one by one");
    assert_eq!(size, (k.width, k.height));
    (k.busy[index], k.last_seq) = (true, seq);
}

fn full_event(k: &mut Known, event: FEv) {
    if let Some(view) = event.view() {
        assert_eq!(view, k.view, "an event on the wrong client's channel");
    }
    match event {
        FEv::Frame(f) => {
            took_frame(k, f.slot, f.seq, (f.width, f.height));
            assert!(f.whole || k.deltas, "a delta for a view without deltas");
            saw(if f.whole { WHOLE } else { DELTA });
            for r in &f.rects {
                let inside = r.x >= 0 && r.y >= 0 && r.width >= 0 && r.height >= 0;
                let (right, bottom) = (r.x + r.width, r.y + r.height);
                assert!(inside && right <= f.width as i32 && bottom <= f.height as i32);
            }
        }
        FEv::Band(b) => {
            let index = b.slot as usize;
            assert!(!k.lost && k.deltas, "a band for a view that takes none");
            assert!((SLOTS_PER_VIEW..SLOTS_WITH_BANDS).contains(&index));
            assert!(
                k.attached[index] && !k.busy[index],
                "a band in a slot that was not free"
            );
            assert_eq!(b.width, k.width);
            k.busy[index] = true;
            saw(BAND);
        }
        FEv::Navigation(_) => saw(ASKED),
        FEv::Dialog(_) => saw(DIALOG),
        FEv::DownloadStarted(_) | FEv::DownloadDone(_) => saw(DOWNLOAD),
        FEv::Lost(_) => {
            k.lost = true;
            k.detach_all();
        }
        _ => {}
    }
}

fn light_event(k: &mut Known, event: Ev) {
    match event {
        Ev::Frame(f) => {
            assert_eq!(f.view, k.view, "an event on the wrong client's channel");
            took_frame(k, f.slot, f.seq, (f.width, f.height));
        }
        Ev::Lost(l) => {
            assert_eq!(l.view, k.view);
            k.lost = true;
            k.detach_all();
        }
        Ev::State(e) => assert_eq!(e.view, k.view),
        Ev::Scroll(e) => assert_eq!(e.view, k.view),
        Ev::Link(e) => assert_eq!(e.view, k.view),
        Ev::Clipboard(e) => assert_eq!(e.view, k.view),
        Ev::Ping => {}
    }
}
