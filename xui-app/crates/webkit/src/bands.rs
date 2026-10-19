//! Overscan bands: rows of the page above and below the view, painted ahead of a scroll.
//!
//! The window shows the picture ahead of the page while it scrolls (`speculate.rs`): a wheel notch is
//! drawn at once, the page answers tens or hundreds of milliseconds later. What that uncovers used to
//! be a checkerboard. With WebKit patch 0030 the page's process paints the document's rows just
//! outside the view whenever it has nothing else to paint, and the UI process hands each band to the
//! window ([`lzw_ui_band`](crate::ffi) -> [`post`] -> [`Bands::insert`]). The window draws them where
//! the checkerboard would be; once the page catches up its own picture covers them again.
//!
//! A band is a preview, not the page: it may be a little stale (the page's process repaints stale
//! bands at most twice a second), and what is fixed to the view (a header) is not in it. Bands are in
//! document coordinates (rows of the page, not of the view), so a scroll does not touch them.

use std::sync::Mutex;

/// A band as the engine's thread posts it: top-down RGBA, opaque, `width * height` pixels, the rows
/// `doc_y .. doc_y + height` of the page painted at the horizontal scroll offset `doc_x`.
#[derive(Debug, Clone)]
pub struct Band {
    pub doc_x: i32,
    pub doc_y: i32,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Bands posted but not collected, at most this many bytes (older ones are dropped first: the page's
/// process paints the nearest rows first, and a window that far behind is redrawn anyway).
const QUEUE_BYTES: usize = 64 << 20;
/// The tallest band taken: the web process paints at most this many rows at a time (patch 0030's
/// chunks); a taller one is not a band it painted.
pub const MAX_BAND_ROWS: u32 = 1024;

static QUEUE: Mutex<Vec<Band>> = Mutex::new(Vec::new());

/// The engine's thread: a band arrived. Returns whether the window must be woken (the queue was
/// empty).
pub fn post(band: Band) -> bool {
    let mut queue = QUEUE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let was_empty = queue.is_empty();
    queue.push(band);
    let mut bytes: usize = queue.iter().map(|band| band.rgba.len()).sum();
    while bytes > QUEUE_BYTES && queue.len() > 1 {
        bytes -= queue.remove(0).rgba.len();
    }
    was_empty
}

/// The window's thread: every band posted since the last call, oldest first.
pub fn take() -> Vec<Band> {
    std::mem::take(
        &mut *QUEUE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    )
}

/// What the window keeps: bands as images, in the order they arrived (a later one covers an earlier
/// one where they overlap, being newer).
pub struct Bands<I> {
    bands: Vec<Kept<I>>,
    doc_x: i32,
    width: u32,
}

impl<I> Default for Bands<I> {
    fn default() -> Self {
        Bands {
            bands: Vec::new(),
            doc_x: 0,
            width: 0,
        }
    }
}

/// A kept band: its rows of the page and its picture.
pub struct Kept<I> {
    pub doc_y: i32,
    pub height: i32,
    pub image: I,
}

/// Bands kept at most, and how far from the view (in view heights) a band may be before it is dropped.
pub const MAX_BANDS: usize = 64;
/// The most pixel bytes kept: a dozen views at 2560x1148, what [`KEEP_VIEWS`] on both sides needs and a
/// little more. Bands come from the page's process, which must not decide this process's memory.
pub const MAX_BAND_BYTES: usize = 144 << 20;
pub const KEEP_VIEWS: i32 = 3;

impl<I> Bands<I> {
    pub fn clear(&mut self) {
        self.bands.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.bands.is_empty()
    }

    pub fn len(&self) -> usize {
        self.bands.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Kept<I>> {
        self.bands.iter()
    }

    /// Keeps a band of `width` pixels painted at the horizontal offset `doc_x`, covering the page's rows
    /// `doc_y .. doc_y + height`. A band of another width or horizontal offset than the ones kept means
    /// the page was laid out or scrolled sideways again: the old ones go.
    pub fn insert(&mut self, doc_x: i32, doc_y: i32, width: u32, height: i32, image: I) {
        if width != self.width || doc_x != self.doc_x {
            self.bands.clear();
            self.width = width;
            self.doc_x = doc_x;
        }
        let (top, bottom) = (doc_y, doc_y.saturating_add(height));
        // What the new band hides entirely is not drawn any more.
        self.bands.retain(|kept| {
            !(kept.doc_y >= top && kept.doc_y.saturating_add(kept.height) <= bottom)
        });
        self.bands.push(Kept {
            doc_y,
            height,
            image,
        });
        let row_bytes = width as usize * 4;
        let bytes = |bands: &[Kept<I>]| -> usize {
            bands.iter().map(|kept| kept.height.max(0) as usize * row_bytes).sum()
        };
        while self.bands.len() > MAX_BANDS || (self.bands.len() > 1 && bytes(&self.bands) > MAX_BAND_BYTES) {
            self.bands.remove(0);
        }
    }

    /// Drops the bands far from the view: `view_top` is the page row at the top of the view, `height`
    /// the view's height.
    pub fn prune(&mut self, view_top: i32, height: i32) {
        let reach = height.saturating_mul(KEEP_VIEWS);
        let (low, high) = (
            view_top.saturating_sub(reach),
            view_top.saturating_add(height).saturating_add(reach),
        );
        self.bands
            .retain(|kept| kept.doc_y.saturating_add(kept.height) > low && kept.doc_y < high);
    }

    /// The width and horizontal offset of the bands kept.
    pub fn geometry(&self) -> (u32, i32) {
        (self.width, self.doc_x)
    }

    /// How many rows past `edge` (a page row) the bands cover without a gap, going down (`down`) or up.
    pub fn reach(&self, edge: i32, down: bool) -> i32 {
        let mut spans: Vec<(i32, i32)> = self
            .bands
            .iter()
            .map(|kept| (kept.doc_y, kept.doc_y.saturating_add(kept.height)))
            .collect();
        spans.sort_unstable();
        let mut covered = edge;
        if down {
            loop {
                let next = spans
                    .iter()
                    .filter(|(top, bottom)| *top <= covered && *bottom > covered)
                    .map(|(_, bottom)| *bottom)
                    .max();
                match next {
                    Some(bottom) => covered = bottom,
                    None => return covered - edge,
                }
            }
        } else {
            loop {
                let next = spans
                    .iter()
                    .filter(|(top, bottom)| *top < covered && *bottom >= covered)
                    .map(|(top, _)| *top)
                    .min();
                match next {
                    Some(top) => covered = top,
                    None => return edge - covered,
                }
            }
        }
    }
}

/// Subtracts the rows `top .. bottom` from a sorted list of disjoint row spans.
pub fn subtract(spans: &mut Vec<(i32, i32)>, top: i32, bottom: i32) {
    if top >= bottom {
        return;
    }
    let mut out = Vec::with_capacity(spans.len() + 1);
    for &(a, b) in spans.iter() {
        if b <= top || a >= bottom {
            out.push((a, b));
            continue;
        }
        if a < top {
            out.push((a, top));
        }
        if b > bottom {
            out.push((bottom, b));
        }
    }
    *spans = out;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bands(spans: &[(i32, i32)]) -> Bands<u32> {
        let mut bands = Bands::default();
        for (n, &(top, bottom)) in spans.iter().enumerate() {
            bands.insert(0, top, 100, bottom - top, n as u32);
        }
        bands
    }

    #[test]
    fn reach_follows_touching_bands_and_stops_at_a_gap() {
        let b = bands(&[(1000, 1200), (1200, 1400), (1500, 1600)]);
        assert_eq!(b.reach(1000, true), 400);
        assert_eq!(b.reach(1100, true), 300);
        assert_eq!(b.reach(1400, true), 0);
        assert_eq!(b.reach(1400, false), 400);
        assert_eq!(b.reach(1000, false), 0);
        assert_eq!(b.reach(1600, false), 100);
    }

    #[test]
    fn a_new_band_drops_what_it_hides_and_a_new_width_drops_all() {
        let mut b = bands(&[(0, 100), (100, 200), (50, 150)]);
        assert_eq!(b.len(), 3);
        b.insert(0, 0, 100, 200, 9);
        assert_eq!(b.len(), 1);
        b.insert(0, 300, 120, 10, 10);
        assert_eq!(b.len(), 1);
        assert_eq!(b.geometry(), (120, 0));
        b.insert(5, 300, 120, 10, 11);
        assert_eq!(b.geometry(), (120, 5));
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn prune_keeps_bands_near_the_view() {
        let mut b = bands(&[(0, 100), (5000, 5100), (20000, 20100)]);
        b.prune(5000, 1000);
        let kept: Vec<i32> = b.iter().map(|k| k.doc_y).collect();
        assert_eq!(kept, vec![5000]);
        b.prune(-1_000_000_000, i32::MAX);
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn hostile_bands_cannot_grow_the_kept_bytes_past_the_cap() {
        let mut b = Bands::default();
        // 1024-row bands 8192 px wide (32 MiB each), none hiding another.
        for n in 0..10 {
            b.insert(0, n * 2000, 8192, 1024, n as u32);
        }
        let kept: usize = b.iter().map(|k| k.height as usize * 8192 * 4).sum();
        assert!(kept <= MAX_BAND_BYTES, "{kept} bytes kept");
        assert_eq!(b.iter().last().map(|k| k.doc_y), Some(18_000), "the newest band stays");
    }

    #[test]
    fn subtract_cuts_spans() {
        let mut spans = vec![(0, 100), (200, 300)];
        subtract(&mut spans, 50, 250);
        assert_eq!(spans, vec![(0, 50), (250, 300)]);
        subtract(&mut spans, 0, 1000);
        assert!(spans.is_empty());
    }

    #[test]
    fn the_queue_keeps_order_and_wakes_once() {
        let _ = take();
        let band = |y| Band {
            doc_x: 0,
            doc_y: y,
            width: 1,
            height: 1,
            rgba: vec![0; 4],
        };
        assert!(post(band(1)));
        assert!(!post(band(2)));
        let got: Vec<i32> = take().iter().map(|b| b.doc_y).collect();
        assert_eq!(got, vec![1, 2]);
        assert!(take().is_empty());
    }
}
