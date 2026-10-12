#![forbid(unsafe_code)]

//! Which part of the picture the canvas shows, and how big.
//!
//! Pure arithmetic, no `Ui`: the canvas maps pointer positions through it and
//! the renderer samples through it, so both agree on which pixel is which.
//!
//! A picture pixel `p` appears at view pixel `p * zoom - origin`. On an axis
//! where the picture is larger than the view the origin scrolls it between 0
//! and the overflow; where it fits, the origin is negative so the picture is
//! centred.

use crate::model::Rect;

/// The smallest zoom, 1/16.
pub const ZOOM_MIN: f64 = 1.0 / 16.0;
/// The largest zoom, 32x.
pub const ZOOM_MAX: f64 = 32.0;
/// The zoom levels Zoom In and Zoom Out step through.
pub const ZOOM_STEPS: [f64; 17] = [
    1.0 / 16.0,
    1.0 / 8.0,
    1.0 / 4.0,
    1.0 / 3.0,
    1.0 / 2.0,
    2.0 / 3.0,
    1.0,
    2.0,
    3.0,
    4.0,
    5.0,
    6.0,
    8.0,
    12.0,
    16.0,
    24.0,
    32.0,
];
/// The zoom from which a pixel grid is drawn.
pub const GRID_ZOOM: f64 = 8.0;

/// A zoom and a scroll position, in view pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    zoom: f64,
    origin: (i32, i32),
}

impl Default for Viewport {
    fn default() -> Viewport {
        Viewport::new()
    }
}

/// `floor(v)` tolerant of the last bit of float error, saturating to `i32`.
fn floor_i32(v: f64) -> i32 {
    (v + 1e-9).floor() as i32
}

impl Viewport {
    /// Actual size (100%), scrolled to the top-left.
    pub fn new() -> Viewport {
        Viewport {
            zoom: 1.0,
            origin: (0, 0),
        }
    }

    /// The zoom factor.
    pub fn zoom(&self) -> f64 {
        self.zoom
    }

    /// The zoom as a whole percent (what the status bar shows).
    pub fn percent(&self) -> u32 {
        (self.zoom * 100.0).round() as u32
    }

    /// How far the picture is scrolled, in view pixels.
    pub fn origin(&self) -> (i32, i32) {
        self.origin
    }

    /// The picture's size in view pixels at this zoom (at least 1 x 1).
    pub fn content_size(&self, doc: (u32, u32)) -> (i32, i32) {
        let scaled = |n: u32| {
            ((f64::from(n) * self.zoom).ceil() as i64).clamp(1, i64::from(i32::MAX)) as i32
        };
        (scaled(doc.0), scaled(doc.1))
    }

    /// Keeps the picture in a `view`-sized window: scrolled within its edges
    /// where it overflows, centred where it fits. Returns whether it moved.
    pub fn clamp(&mut self, doc: (u32, u32), view: (i32, i32)) -> bool {
        let content = self.content_size(doc);
        let axis = |origin: i32, content: i32, view: i32| {
            if content <= view {
                -((view - content) / 2)
            } else {
                origin.clamp(0, content - view)
            }
        };
        let before = self.origin;
        self.origin = (
            axis(self.origin.0, content.0, view.0),
            axis(self.origin.1, content.1, view.1),
        );
        self.origin != before
    }

    /// Scrolls by `(dx, dy)` view pixels; returns whether anything moved.
    pub fn pan_by(&mut self, dx: i32, dy: i32, doc: (u32, u32), view: (i32, i32)) -> bool {
        let before = self.origin;
        self.origin = (
            self.origin.0.saturating_add(dx),
            self.origin.1.saturating_add(dy),
        );
        self.clamp(doc, view);
        self.origin != before
    }

    /// Zooms to `zoom` (clamped to the supported range) keeping the picture
    /// point under the view pixel `anchor` where it is.
    pub fn zoom_to(&mut self, zoom: f64, anchor: (i32, i32), doc: (u32, u32), view: (i32, i32)) {
        let zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        let under = (
            (f64::from(anchor.0) + f64::from(self.origin.0)) / self.zoom,
            (f64::from(anchor.1) + f64::from(self.origin.1)) / self.zoom,
        );
        self.zoom = zoom;
        self.origin = (
            (under.0 * zoom - f64::from(anchor.0)).round() as i32,
            (under.1 * zoom - f64::from(anchor.1)).round() as i32,
        );
        self.clamp(doc, view);
    }

    /// The next zoom step above the current zoom (or the maximum).
    pub fn next_zoom_in(&self) -> f64 {
        ZOOM_STEPS
            .iter()
            .copied()
            .find(|z| *z > self.zoom * 1.0001)
            .unwrap_or(ZOOM_MAX)
    }

    /// The next zoom step below the current zoom (or the minimum).
    pub fn next_zoom_out(&self) -> f64 {
        ZOOM_STEPS
            .iter()
            .rev()
            .copied()
            .find(|z| *z < self.zoom / 1.0001)
            .unwrap_or(ZOOM_MIN)
    }

    /// Zooms in one step around `anchor`.
    pub fn zoom_in(&mut self, anchor: (i32, i32), doc: (u32, u32), view: (i32, i32)) {
        self.zoom_to(self.next_zoom_in(), anchor, doc, view);
    }

    /// Zooms out one step around `anchor`.
    pub fn zoom_out(&mut self, anchor: (i32, i32), doc: (u32, u32), view: (i32, i32)) {
        self.zoom_to(self.next_zoom_out(), anchor, doc, view);
    }

    /// 100%, keeping the centre of the view where it is.
    pub fn actual_size(&mut self, doc: (u32, u32), view: (i32, i32)) {
        self.zoom_to(1.0, (view.0 / 2, view.1 / 2), doc, view);
    }

    /// The largest zoom (within range) at which the whole picture fits.
    pub fn fit(&mut self, doc: (u32, u32), view: (i32, i32)) {
        let fit_w = f64::from(view.0.max(1)) / f64::from(doc.0.max(1));
        let fit_h = f64::from(view.1.max(1)) / f64::from(doc.1.max(1));
        self.zoom = fit_w.min(fit_h).clamp(ZOOM_MIN, ZOOM_MAX);
        self.origin = (0, 0);
        self.clamp(doc, view);
    }

    /// The picture pixel under the view pixel `(x, y)`; outside the picture
    /// this is outside `0..width`/`0..height`.
    pub fn to_doc(&self, x: i32, y: i32) -> (i32, i32) {
        let at = |v: i32, origin: i32| floor_i32((f64::from(v) + f64::from(origin)) / self.zoom);
        (at(x, self.origin.0), at(y, self.origin.1))
    }

    /// The view pixel of the top-left corner of picture pixel `(x, y)`.
    pub fn to_view(&self, x: i32, y: i32) -> (i32, i32) {
        let at = |v: i32, origin: i32| (f64::from(v) * self.zoom).round() as i32 - origin;
        (at(x, self.origin.0), at(y, self.origin.1))
    }

    /// The picture pixels visible in a `view`-sized window, clipped to `doc`.
    pub fn visible(&self, doc: (u32, u32), view: (i32, i32)) -> Rect {
        let (x0, y0) = self.to_doc(0, 0);
        // The exclusive end: the first picture pixel wholly past the view.
        let end = |v: i32, origin: i32| {
            ((f64::from(v) + f64::from(origin)) / self.zoom - 1e-9).ceil() as i32
        };
        Rect {
            x0,
            y0,
            x1: end(view.0, self.origin.0),
            y1: end(view.1, self.origin.1),
        }
        .intersect(Rect::new(0, 0, doc.0, doc.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: (u32, u32) = (320, 240);
    const VIEW: (i32, i32) = (800, 600);

    #[test]
    fn at_actual_size_view_and_picture_pixels_coincide() {
        let vp = Viewport::new();
        assert_eq!(vp.to_doc(33, 44), (33, 44));
        assert_eq!(vp.to_view(33, 44), (33, 44));
        assert_eq!(vp.percent(), 100);
    }

    #[test]
    fn zoomed_in_every_picture_pixel_is_a_square() {
        let mut vp = Viewport::new();
        vp.zoom_to(8.0, (0, 0), DOC, (100, 100));
        assert_eq!(vp.percent(), 800);
        assert_eq!(vp.to_doc(0, 0), (0, 0));
        assert_eq!(vp.to_doc(7, 7), (0, 0));
        assert_eq!(vp.to_doc(8, 15), (1, 1));
        assert_eq!(vp.to_view(3, 2), (24, 16));
        // Scrolled by 10 view pixels, the mapping shifts.
        vp.pan_by(10, 0, DOC, (100, 100));
        assert_eq!(vp.to_doc(0, 0), (1, 0));
        assert_eq!(vp.to_view(1, 0), (-2, 0));
    }

    #[test]
    fn zoomed_out_several_pixels_share_a_view_pixel() {
        let mut vp = Viewport::new();
        vp.zoom_to(0.25, (0, 0), DOC, (40, 30));
        assert_eq!(vp.to_doc(10, 10), (40, 40));
        assert_eq!(vp.content_size(DOC), (80, 60));
        let mut third = Viewport::new();
        third.zoom_to(1.0 / 3.0, (0, 0), (300, 300), (50, 50));
        assert_eq!(third.to_doc(5, 5), (15, 15), "no float slip at 1/3");
    }

    #[test]
    fn the_point_under_the_pointer_stays_put() {
        let mut vp = Viewport::new();
        vp.zoom_to(4.0, (0, 0), (2000, 2000), VIEW);
        vp.pan_by(900, 700, (2000, 2000), VIEW);
        let anchor = (300, 200);
        let before = vp.to_doc(anchor.0, anchor.1);
        vp.zoom_in(anchor, (2000, 2000), VIEW);
        assert_eq!(vp.zoom(), 5.0);
        assert_eq!(vp.to_doc(anchor.0, anchor.1), before);
        vp.zoom_out(anchor, (2000, 2000), VIEW);
        assert_eq!(vp.to_doc(anchor.0, anchor.1), before);
    }

    #[test]
    fn steps_run_from_a_sixteenth_to_32x() {
        let mut vp = Viewport::new();
        for _ in 0..30 {
            vp.zoom_in((0, 0), DOC, VIEW);
        }
        assert_eq!(vp.zoom(), 32.0);
        for _ in 0..30 {
            vp.zoom_out((0, 0), DOC, VIEW);
        }
        assert_eq!(vp.zoom(), 1.0 / 16.0);
        // From an arbitrary fit zoom the next step is still a step.
        vp.fit((1000, 1000), (300, 300));
        assert!((vp.zoom() - 0.3).abs() < 1e-9);
        assert!((vp.next_zoom_in() - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(vp.next_zoom_out(), 0.25);
    }

    #[test]
    fn scrolling_stops_at_the_picture_edges() {
        let mut vp = Viewport::new();
        vp.clamp(DOC, VIEW);
        assert_eq!(vp.origin(), (-240, -180), "it fits: centred");
        assert!(
            !vp.pan_by(50, 50, DOC, VIEW),
            "it all fits: nothing scrolls"
        );
        vp.zoom_to(8.0, (0, 0), DOC, VIEW);
        vp.pan_by(100_000, 100_000, DOC, VIEW);
        assert_eq!(vp.origin(), (320 * 8 - 800, 240 * 8 - 600));
        vp.pan_by(-100_000, -100_000, DOC, VIEW);
        assert_eq!(vp.origin(), (0, 0));
    }

    #[test]
    fn fit_and_actual_size() {
        let mut vp = Viewport::new();
        vp.fit((1600, 1200), VIEW);
        assert_eq!(vp.zoom(), 0.5);
        assert_eq!(vp.visible((1600, 1200), VIEW), Rect::new(0, 0, 1600, 1200));
        vp.actual_size((1600, 1200), VIEW);
        assert_eq!(vp.percent(), 100);
        // Zooming keeps the centre of the view: the middle of the picture.
        assert_eq!(
            vp.visible((1600, 1200), VIEW),
            Rect::new(400, 300, 800, 600)
        );
    }

    #[test]
    fn a_picture_that_fits_is_centred_on_that_axis_only() {
        let mut vp = Viewport::new();
        // 320 x 240 at 200% is 640 wide (fits 800) and 480 tall (overflows 300).
        vp.zoom_to(2.0, (0, 0), DOC, (800, 300));
        assert_eq!(vp.origin(), (-80, 0));
        assert_eq!(vp.to_view(0, 0), (80, 0));
        assert_eq!(vp.to_doc(80, 0), (0, 0));
        assert_eq!(vp.to_doc(79, 0).0, -1, "left of the picture");
        vp.pan_by(10, 1000, DOC, (800, 300));
        assert_eq!(vp.origin(), (-80, 180));
    }

    #[test]
    fn fit_leaves_no_margin_on_the_tight_axis() {
        let mut vp = Viewport::new();
        vp.fit(DOC, VIEW);
        // 800 / 320 = 2.5, 600 / 240 = 2.5: both axes tight.
        assert_eq!(vp.origin(), (0, 0));
        vp.fit((100, 400), VIEW);
        assert_eq!(vp.zoom(), 1.5);
        assert_eq!(vp.origin(), (-325, 0), "150 px wide in 800: centred");
    }

    #[test]
    fn zooming_out_of_an_overflow_keeps_the_anchor_while_it_still_overflows() {
        let mut vp = Viewport::new();
        vp.zoom_to(4.0, (0, 0), (2000, 2000), VIEW);
        vp.pan_by(900, 700, (2000, 2000), VIEW);
        let anchor = (300, 200);
        let before = vp.to_doc(anchor.0, anchor.1);
        vp.zoom_out(anchor, (2000, 2000), VIEW);
        assert_eq!(vp.to_doc(anchor.0, anchor.1), before);
    }

    #[test]
    fn hostile_input_never_panics() {
        let mut vp = Viewport::new();
        vp.zoom_to(f64::MAX, (i32::MAX, i32::MIN), DOC, VIEW);
        vp.pan_by(i32::MAX, i32::MIN, DOC, VIEW);
        vp.zoom_to(0.0, (0, 0), DOC, (0, 0));
        let _ = vp.to_doc(i32::MIN, i32::MAX);
        let _ = vp.to_view(i32::MAX, i32::MIN);
        let _ = vp.visible((1, 1), (0, 0));
    }
}
