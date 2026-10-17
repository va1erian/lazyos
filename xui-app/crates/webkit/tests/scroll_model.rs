//! The scroll path against a model of WebKit's backing store, pixel for pixel.
//!
//! WebKit's classic drawing area applies an update to its backing store by moving the scrolled
//! area (`BackingStore::scroll`: what was inside `area` goes `(dx, dy)` further; what the move
//! uncovers keeps its old pixels) and then painting the update's rectangles, which include the
//! uncovered part. The UI process learns each update (`Pending::note_update`), paints from the
//! store when it chooses (`take_plan`), hands the window a delta or a whole frame through the
//! mailbox, and the window applies them to its parts. Whatever the order and the lag, the picture
//! the window draws must equal the backing store as it was when the engine last painted.

use xui_core::Image;
use xui_webkit::damage::{IRect, Pending, Plan, Scroll};
use xui_webkit::mailbox::{Delta, Mailbox, Patch};
use xui_webkit::parts::Parts;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: i32) -> i32 {
        (self.next() % n.max(1) as u64) as i32
    }

    fn range(&mut self, low: i32, high: i32) -> i32 {
        low + self.below(high - low + 1)
    }
}

/// WebKit's backing store: `w * h` pixels.
#[derive(Clone, PartialEq)]
struct Store {
    w: i32,
    h: i32,
    px: Vec<u32>,
}

impl Store {
    fn new(w: i32, h: i32, rng: &mut Rng) -> Store {
        Store {
            w,
            h,
            px: (0..w * h).map(|_| rng.next() as u32).collect(),
        }
    }

    fn view(&self) -> IRect {
        IRect::new(0, 0, self.w, self.h)
    }

    /// `BackingStore::scroll`: only the target area is written, from the source it came from.
    fn scroll(&mut self, area: IRect, dx: i32, dy: i32) {
        let target = area
            .translate(dx, dy)
            .intersect(&area)
            .intersect(&self.view());
        if target.is_empty() {
            return;
        }
        let from: Vec<u32> = (target.y0..target.y1)
            .flat_map(|y| (target.x0..target.x1).map(move |x| (x, y)))
            .map(|(x, y)| self.px[((y - dy) * self.w + (x - dx)) as usize])
            .collect();
        let mut taken = from.into_iter();
        for y in target.y0..target.y1 {
            for x in target.x0..target.x1 {
                self.px[(y * self.w + x) as usize] = taken.next().unwrap();
            }
        }
    }

    fn paint(&mut self, rect: IRect, rng: &mut Rng) {
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                self.px[(y * self.w + x) as usize] = rng.next() as u32;
            }
        }
    }

    fn rgba(&self, rect: IRect) -> Vec<u8> {
        let mut out = Vec::new();
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                out.extend_from_slice(&self.px[(y * self.w + x) as usize].to_le_bytes());
            }
        }
        out
    }
}

/// The engine's side: what it knows of the updates, and the store it paints from.
struct Engine {
    store: Store,
    pending: Pending,
    /// The store as of the last paint the window will (or did) get.
    painted: Store,
    /// A delta was refused, so the window may have lost deltas before it: its picture is stale until the next whole frame.
    stale: bool,
}

impl Engine {
    fn random_rect(&self, rng: &mut Rng) -> IRect {
        let (w, h) = (self.store.w, self.store.h);
        let (x, y) = (rng.below(w), rng.below(h));
        IRect::new(
            x,
            y,
            rng.range(1, (w - x).min(12)),
            rng.range(1, (h - y).min(12)),
        )
    }

    /// One update from the page's process.
    fn update(&mut self, rng: &mut Rng) {
        let (w, h) = (self.store.w, self.store.h);
        match rng.below(10) {
            0..=6 => {
                let area = match rng.below(10) {
                    0..=6 => IRect::new(0, 0, w - 3, h),
                    7..=8 => IRect::new(0, 0, w, h),
                    _ => self.random_rect(rng),
                };
                let big = rng.below(12) == 0;
                let dy = if big {
                    rng.range(-h, h)
                } else {
                    rng.range(-9, 9)
                };
                let dx = if rng.below(5) == 0 {
                    rng.range(-4, 4)
                } else {
                    0
                };
                self.store.scroll(area, dx, dy);
                // What the scroll exposed is repainted in the same update (`scrollRepaintRegion`).
                let mut rects = Vec::new();
                area.subtract(&area.translate(dx, dy), &mut rects);
                for _ in 0..rng.below(3) {
                    rects.push(self.random_rect(rng));
                }
                if rng.below(2) == 0 {
                    rects.push(IRect::new(w - 3, 0, 3, h));
                }
                for rect in &rects {
                    self.store.paint(rect.intersect(&self.store.view()), rng);
                }
                self.pending.note_update(area, dx, dy, &rects);
            }
            7..=8 => {
                let rect = self.random_rect(rng);
                self.store.paint(rect, rng);
                self.pending.note_rect(rect);
            }
            _ => {
                let rects = [self.random_rect(rng), self.random_rect(rng)];
                for rect in rects {
                    self.store.paint(rect, rng);
                }
                self.pending.note_update(IRect::default(), 0, 0, &rects);
            }
        }
    }

    /// A paint: take the plan, read the store, hand the window what it needs.
    fn paint(&mut self, mailbox: &Mailbox) {
        let (w, h) = (self.store.w, self.store.h);
        match self.pending.take_plan() {
            None => {}
            Some(Plan::Full) => {
                mailbox.post_frame(w as u32, h as u32, &self.store.rgba(self.store.view()));
                self.painted = self.store.clone();
                self.stale = false;
            }
            Some(Plan::Delta { scroll, rects }) => {
                let patches = rects
                    .iter()
                    .map(|rect| Patch {
                        rect: *rect,
                        rgba: self.store.rgba(*rect),
                    })
                    .collect();
                let delta = Delta {
                    size: (w as u32, h as u32),
                    scroll,
                    patches,
                };
                if mailbox.post_delta(delta) {
                    self.pending.note_full();
                    self.stale = true;
                } else {
                    self.painted = self.store.clone();
                }
            }
        }
    }
}

/// The window's side: collects from the mailbox and applies.
struct Window {
    picture: Parts,
    spare: Vec<u8>,
    /// The picture follows the engine (nothing was refused or dropped since a whole frame).
    synced: bool,
}

impl Window {
    fn collect(&mut self, mailbox: &Mailbox) {
        let Some(update) = mailbox.take_update(std::mem::take(&mut self.spare)) else {
            return;
        };
        if let Some(frame) = update.full {
            let image = Image::from_rgba(frame.width, frame.height, frame.rgba).unwrap();
            self.spare = self.picture.set_full(image);
            self.synced = true;
        }
        for delta in update.deltas {
            if !self.picture.apply(delta) {
                mailbox.request_full();
                self.synced = false;
                break;
            }
        }
        if self.picture.is_fragmented() {
            mailbox.request_full();
            self.synced = false;
        }
    }

    fn shows(&self, store: &Store) -> bool {
        let mut buffer = vec![0u32; (store.w * store.h) as usize];
        self.picture
            .render_into(&mut buffer, store.w as usize, store.h as usize);
        buffer == store.px
    }
}

fn run(seed: u64, w: i32, h: i32, steps: usize) -> (usize, usize) {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let store = Store::new(w, h, &mut rng);
    let mut engine = Engine {
        painted: store.clone(),
        store,
        pending: Pending::new(w, h),
        stale: false,
    };
    let mailbox = Mailbox::default();
    let mut window = Window {
        picture: Parts::default(),
        spare: Vec::new(),
        synced: false,
    };
    let (mut deltas_seen, mut fulls_seen) = (0, 0);
    engine.paint(&mailbox);
    window.collect(&mailbox);
    for step in 0..steps {
        for _ in 0..rng.range(1, 3) {
            engine.update(&mut rng);
        }
        if rng.below(3) != 0 {
            engine.paint(&mailbox);
        }
        if rng.below(2) == 0 {
            let had = window.picture.len();
            window.collect(&mailbox);
            if window.picture.len() != had {
                deltas_seen += 1;
            }
            if window.synced && !engine.stale {
                assert!(
                    window.shows(&engine.painted),
                    "seed {seed} step {step}: the picture is not the store"
                );
            }
        }
    }
    // However it went, a whole frame makes the picture the store.
    engine.pending.note_full();
    engine.paint(&mailbox);
    window.collect(&mailbox);
    fulls_seen += 1;
    assert!(
        window.shows(&engine.store),
        "seed {seed}: after a whole frame"
    );
    (deltas_seen, fulls_seen)
}

#[test]
fn the_picture_is_always_the_store_whatever_the_order() {
    let mut applied = 0;
    for seed in 1..=400u64 {
        let (w, h) = [(37, 29), (64, 48), (23, 71), (101, 17)][seed as usize % 4];
        applied += run(seed, w, h, 80).0;
    }
    assert!(
        applied > 1000,
        "the runs hardly applied any delta ({applied})"
    );
}

#[test]
fn a_scroll_of_the_whole_view_in_steps_is_all_deltas() {
    // The plain case the change is for: a page scrolling in the steady state sends a delta each
    // paint, never a whole frame, and the picture stays right.
    let mut rng = Rng(7);
    let (w, h) = (80, 60);
    let store = Store::new(w, h, &mut rng);
    let mut engine = Engine {
        painted: store.clone(),
        store,
        pending: Pending::new(w, h),
        stale: false,
    };
    let mailbox = Mailbox::default();
    let mut window = Window {
        picture: Parts::default(),
        spare: Vec::new(),
        synced: false,
    };
    engine.paint(&mailbox);
    window.collect(&mailbox);
    let area = IRect::new(0, 0, w - 4, h);
    let mut full_frames = 0;
    for step in 0..200 {
        let dy = if (step / 25) % 2 == 0 { -3 } else { 3 };
        engine.store.scroll(area, 0, dy);
        let mut exposed = Vec::new();
        area.subtract(&area.translate(0, dy), &mut exposed);
        exposed.push(IRect::new(w - 4, 0, 4, h));
        for rect in &exposed {
            engine.store.paint(*rect, &mut rng);
        }
        engine.pending.note_update(area, 0, dy, &exposed);
        let before = engine.pending.clone_for_test();
        engine.paint(&mailbox);
        if matches!(before, Some(Plan::Full)) {
            full_frames += 1;
        }
        window.collect(&mailbox);
        assert!(window.shows(&engine.painted), "step {step}");
    }
    // A whole frame now and then (the parts pile up: every step leaves a strip behind) but most
    // steps are deltas.
    assert!(full_frames < 20, "{full_frames} whole frames in 200 steps");
}

trait CloneForTest {
    fn clone_for_test(&self) -> Option<Plan>;
}

impl CloneForTest for Pending {
    fn clone_for_test(&self) -> Option<Plan> {
        // A peek: the plan this pending state would give, without taking it.
        let mut copy = self.clone();
        copy.take_plan()
    }
}

#[test]
fn scroll_type_is_public_and_plain() {
    let scroll = Scroll {
        area: IRect::new(0, 0, 10, 10),
        dx: 1,
        dy: -2,
    };
    assert_eq!(scroll, scroll);
}
