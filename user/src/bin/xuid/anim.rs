//! Classic Mac-style window animation: an inverted (XOR) wireframe "zoom
//! rectangle" that flies between a window and its icon (taskbar entry) while
//! the window itself is not drawn. Every move is two-stepped: the window
//! shrinks to an icon-sized rectangle centred on it, which then travels to the
//! target. The compositor is single-threaded, so the animation is a short
//! blocking loop, well under a quarter second per step.
//!
//! The motion is a function of time, not of frames: each phase lasts
//! [`PHASE_NS`] whatever the frame rate, and frames are paced at 60 Hz on
//! the monotonic clock ([`FRAME_NS`], nanosecond sleeps). A frame that takes
//! longer than its slot (a 2x screen, a busy machine) makes the next one
//! later and further along, never the whole animation longer.

use alloc::vec;
use alloc::vec::Vec;
use user::messenger::display::{Canvas, Rect};
use user::sys;

use super::compositor::Compositor;
use super::cursor::present_cursor;
use super::layout::icon_rect;
use super::region::Region;
use super::theme::px;

/// How long the leading outline takes from one rectangle to the other.
const PHASE_NS: u64 = 100_000_000;
/// One frame at 60 Hz.
const FRAME_NS: u64 = 16_666_667;
/// Outlines drawn per frame: the leading rectangle and two trailing ones.
const TRAIL: i32 = 3;
/// How far each trailing outline lags the previous one.
const TRAIL_LAG_NS: u64 = 10_000_000;
/// Full scale of an outline's progress along the move ([`lerp`]).
const FULL: i32 = 1024;
/// Outline thickness in pixels. Outlines are drawn inverted (XOR), so they
/// show on any background.
const LINE: i32 = 2;

/// The outline thickness at the UI scale.
fn line() -> i32 {
    px(LINE)
}

impl Compositor {
    /// Fly a wireframe from `from` to `to` over the screen as composed from
    /// the surfaces. The caller keeps the animated window out of the visible
    /// set (minimized) for the duration and repaints the full screen
    /// afterwards, which erases the last outline. Each frame recomposes its
    /// damage and then XORs the outlines onto the clean pixels, so nothing
    /// depends on erasing an earlier outline by redrawing it.
    ///
    /// The loop blocks the main loop, so every frame also reads the pending
    /// input into the held queue (`held.rs`) and moves the cursor to the
    /// newest pointer position, drawn above the outlines: the pointer never
    /// freezes during an animation, and no event is lost or reordered.
    ///
    /// With animations switched off (`sys/ui/anim`) this draws nothing: every
    /// caller already repaints the final state, so the change is instant.
    pub(super) fn zoom(&mut self, from: Rect, to: Rect) {
        if !self.themefeed.animations() {
            return;
        }
        let full = self.full();
        // What the last frame drew and must be restored: at first the whole
        // starting rectangle, which erases the window that was just hidden;
        // afterwards only the outline strips. Recomposing just the strips
        // keeps a frame's cost proportional to the outlines' length, not to
        // the window's area, which a 2x screen quadruples (docs/hidpi-plan.md).
        let mut previous = [Rect::new(0, 0, 0, 0); STRIPS];
        previous[0] = from.intersect(full);
        let start = sys::monotonic_ns();
        let total = PHASE_NS + TRAIL_LAG_NS * (TRAIL as u64 - 1);
        let mut frame = 1u64;
        loop {
            // This frame shows the motion at its own presentation time, and
            // the last one shows every outline at `to`.
            let shown_at = (frame * FRAME_NS).min(total);
            self.hold_pending_input();
            let mut rects = [Rect::new(0, 0, 0, 0); TRAIL as usize];
            for (index, slot) in rects.iter_mut().enumerate() {
                if let Some(at) = progress(shown_at, index as u64) {
                    *slot = lerp(from, to, at);
                }
            }
            let strips = trail_strips(&rects, full);
            self.draw_frame(&previous, &strips, &rects);
            previous = strips;
            // Pace to the frame's slot; a late frame skips the slots it
            // missed instead of slowing the motion down.
            let _ = sys::sleep_until_ns(start + shown_at);
            if shown_at >= total {
                break;
            }
            let elapsed = sys::monotonic_ns().saturating_sub(start);
            frame = (frame + 1).max(elapsed / FRAME_NS + 1);
        }
    }

    /// Draw one animation frame: recompose what the last frame drew
    /// (`previous`) and what this one covers (`strips`), XOR the trail
    /// `rects` onto the clean pixels, put the cursor back on top and present
    /// it all.
    fn draw_frame(
        &mut self,
        previous: &[Rect; STRIPS],
        strips: &[Rect; STRIPS],
        rects: &[Rect; TRAIL as usize],
    ) {
        let full = self.full();
        // Clean pixels first everywhere this frame touches: XOR needs them,
        // so the cursor overlay is lifted while the frame is drawn.
        let lifted = self.cursor.lift(&mut self.screen);
        for damage in previous.iter().chain(strips) {
            if !damage.is_empty() {
                self.compose(*damage);
            }
        }
        // Overlapping outlines (equal ones as the eased motion settles, but
        // also distinct ones that share an edge) would cancel under XOR,
        // so the trail is drawn as disjoint pieces, each pixel inverted once.
        // Every piece lies inside this frame's strips, recomposed above.
        let mut pieces = [Rect::new(0, 0, 0, 0); MAX_PIECES];
        let count = trail_pieces(rects, &mut pieces);
        for piece in &pieces[..count] {
            self.screen.invert(*piece, full);
        }
        // The cursor stays above the outlines, at the newest pointer.
        let stamped = self.stamp_cursor();
        for shown in previous.iter().chain(strips) {
            if !shown.is_empty() {
                let _ = sys::display_present(shown.x, shown.y, shown.w, shown.h);
            }
        }
        present_cursor(lifted, stamped);
    }

    /// Iconify in two phases: the window shrinks in place to a taskbar-entry
    /// sized wireframe, which then slides to the entry. `id` must already be
    /// marked minimized so the window is not composed under the wireframe.
    pub(super) fn iconify(&mut self, id: u64) {
        let Some((window, icon, _)) = self.phases(id) else {
            return;
        };
        self.zoom_two_step(id, window, icon);
    }

    /// The reverse of [`Compositor::iconify`]: the wireframe slides from the
    /// entry to the window's centre, then grows to the window, before it is
    /// shown.
    pub(super) fn deiconify(&mut self, id: u64) {
        let Some((window, icon, small)) = self.phases(id) else {
            return;
        };
        self.zoom(icon, small);
        self.zoom(small, window);
    }

    /// Animate a new window opening: from `origin` (the on-screen rectangle
    /// the app hinted at, e.g. the folder tile just double-clicked) to the
    /// window in two steps through an icon-sized rectangle centred on the
    /// window, or from its taskbar entry when there is no hint.
    pub(super) fn open_zoom(&mut self, id: u64, origin: Option<Rect>) {
        let Some(from) = origin else {
            self.deiconify(id);
            return;
        };
        if let Some((window, _, small)) = self.phases(id) {
            self.zoom(from, small);
            self.zoom(small, window);
        }
    }

    /// Move or resize `id` from `from` to `to` in two steps, like iconify:
    /// `from` shrinks to an icon-sized rectangle centred on it, which then
    /// travels to `to`. Used by maximize and restore; `id` must already be
    /// hidden (minimized) for the duration.
    pub(super) fn zoom_two_step(&mut self, id: u64, from: Rect, to: Rect) {
        let icon = self.icon(id);
        let small = small_rect(from, icon.w, icon.h, self.full());
        self.zoom(from, small);
        self.zoom(small, to);
    }

    /// Hide or show surface `id` without any other side effect.
    pub(super) fn set_minimized(&mut self, id: u64, minimized: bool) {
        if let Some(surface) = self.surfaces.iter_mut().find(|surface| surface.id == id) {
            surface.minimized = minimized;
        }
    }

    /// Surface `id`'s icon (taskbar entry) rectangle.
    fn icon(&self, id: u64) -> Rect {
        icon_rect(&self.surfaces, self.screen.height(), id)
    }

    /// The window, its icon rectangle, and the icon-sized rectangle centred on
    /// the window that joins them.
    fn phases(&self, id: u64) -> Option<(Rect, Rect, Rect)> {
        let window = self
            .surfaces
            .iter()
            .find(|surface| surface.id == id)?
            .window();
        let icon = self.icon(id);
        Some((
            window,
            icon,
            small_rect(window, icon.w, icon.h, self.full()),
        ))
    }
}

/// The `w` x `h` rectangle centred on `window`, kept inside `bounds`: a
/// mostly off-screen window would otherwise centre it off screen too, and the
/// animation would fly off the edge.
fn small_rect(window: Rect, w: i32, h: i32, bounds: Rect) -> Rect {
    let small = Rect::new(
        window.x + (window.w - w) / 2,
        window.y + (window.h - h) / 2,
        w,
        h,
    );
    super::geometry::clamp_into(small, bounds)
}

/// How far along the move trail outline `index` is `elapsed` nanoseconds
/// in, out of [`FULL`]; `None` before it has started (nothing drawn).
fn progress(elapsed: u64, index: u64) -> Option<i32> {
    let own = elapsed
        .checked_sub(index * TRAIL_LAG_NS)
        .filter(|t| *t > 0)?;
    Some((own.min(PHASE_NS) * FULL as u64 / PHASE_NS) as i32)
}

/// The rectangle `at`/[`FULL`] of the way from `from` to `to`, eased out so
/// the motion starts fast and settles like the classic zoom.
fn lerp(from: Rect, to: Rect, at: i32) -> Rect {
    let t = at.clamp(0, FULL);
    let eased = FULL - (FULL - t) * (FULL - t) / FULL;
    let mix = |a: i32, b: i32| a + (b - a) * eased / FULL;
    Rect::new(
        mix(from.x, to.x),
        mix(from.y, to.y),
        mix(from.w, to.w).max(2),
        mix(from.h, to.h).max(2),
    )
}

/// Strips one frame's trail occupies: four per outline.
const STRIPS: usize = TRAIL as usize * 4;

/// The screen strips the outlines of `rects` cover, each with one pixel of
/// slack so the outlines' edges are always inside; empty for an
/// unused trail slot.
fn trail_strips(rects: &[Rect; TRAIL as usize], full: Rect) -> [Rect; STRIPS] {
    let mut out = [Rect::new(0, 0, 0, 0); STRIPS];
    for (index, rect) in rects.iter().enumerate().filter(|(_, r)| !r.is_empty()) {
        for (side, strip) in outline_strips(*rect).into_iter().enumerate() {
            let grown = Rect::new(strip.x - 1, strip.y - 1, strip.w + 2, strip.h + 2);
            out[index * 4 + side] = grown.intersect(full);
        }
    }
    out
}

/// The four disjoint strips of a hollow rectangle (top, bottom, then left and
/// right between them). They must not overlap: XOR drawing would cancel the
/// overlap, leaving gaps in the corners.
fn outline_strips(rect: Rect) -> [Rect; 4] {
    let t = line().min(rect.w / 2).min(rect.h / 2).max(1);
    let inner = (rect.h - 2 * t).max(0);
    [
        Rect::new(rect.x, rect.y, rect.w, t),
        Rect::new(rect.x, rect.y + rect.h - t, rect.w, t),
        Rect::new(rect.x, rect.y + t, t, inner),
        Rect::new(rect.x + rect.w - t, rect.y + t, t, inner),
    ]
}

/// The most disjoint pieces a trail is cut into. Three outlines of four
/// strips each need far fewer; a full buffer only drops pieces of the
/// outline, never memory (frames must not allocate: the bump allocator never
/// reclaims).
const MAX_PIECES: usize = 64;

/// The union of the trail's outlines as pairwise-disjoint rectangles written
/// to `out`, so inverting each once never cancels where outlines overlap;
/// returns how many. Empty rectangles are skipped.
fn trail_pieces(rects: &[Rect], out: &mut [Rect; MAX_PIECES]) -> usize {
    let mut count = 0;
    for rect in rects.iter().filter(|rect| !rect.is_empty()) {
        for strip in outline_strips(*rect) {
            let mut fresh = Region::new(strip);
            for done in &out[..count] {
                fresh.subtract(*done);
            }
            for piece in fresh.rects() {
                if count < MAX_PIECES {
                    out[count] = *piece;
                    count += 1;
                }
            }
        }
    }
    count
}

/// Draw a hollow rectangle by inverting the pixels under it, so it is visible
/// on any background.
pub(super) fn outline(screen: &mut Canvas, rect: Rect, clip: Rect) {
    for strip in outline_strips(rect) {
        screen.invert(strip, clip);
    }
}

/// Boot check of the animation geometry and the XOR outline:
/// `XUID:ANIM:PASS` or `XUID:ANIM:FAIL`.
pub(super) fn selftest_anim() -> &'static str {
    let from = Rect::new(10, 20, 200, 100);
    let to = Rect::new(300, 200, 400, 300);
    let ends = lerp(from, to, FULL) == to && lerp(from, to, 0) == from;
    // The eased motion starts past the linear midpoint.
    let mid = lerp(from, to, FULL / 2);
    let eased = mid.x > (from.x + to.x) / 2 && mid.w > (from.w + to.w) / 2;
    // Time drives it: the trail starts staggered and ends together.
    let total = PHASE_NS + TRAIL_LAG_NS * (TRAIL as u64 - 1);
    let timed = progress(0, 0).is_none()
        && progress(TRAIL_LAG_NS / 2, 1).is_none()
        && progress(PHASE_NS / 2, 0) == Some(FULL / 2)
        && (0..TRAIL as u64).all(|index| progress(total, index) == Some(FULL));

    // The icon-sized middle rectangle is centred on the window and clamped.
    let bounds = Rect::new(0, 0, 800, 600);
    let centred =
        small_rect(Rect::new(100, 100, 300, 200), 100, 20, bounds) == Rect::new(200, 190, 100, 20);
    let off = small_rect(Rect::new(-500, 100, 300, 200), 100, 20, bounds);
    let clamped = off.x >= bounds.x && off.x + off.w <= bounds.x + bounds.w;

    // The outline strips tile the frame's ring exactly: none overlap.
    let frame = Rect::new(5, 6, 30, 20);
    let strips = outline_strips(frame);
    let area: i32 = strips.iter().map(|s| s.w * s.h).sum();
    let disjoint = strips
        .iter()
        .enumerate()
        .all(|(i, a)| strips[i + 1..].iter().all(|b| a.intersect(*b).is_empty()));
    let tiled = area == 30 * 20 - (30 - 2 * line()) * (20 - 2 * line());

    // Inverting touches exactly the strips and is its own inverse.
    let (w, h) = (40, 32);
    let mut buf = vec![0x40u8; (w * h * 4) as usize];
    let clip = Rect::new(0, 0, w, h);
    // SAFETY: `buf` holds w*h*4 bytes and outlives each `canvas`, which is the
    // only thing touching it while it lives.
    let mut canvas = unsafe { Canvas::new(buf.as_mut_ptr() as u64, w, h) };
    outline(&mut canvas, frame, clip);
    let rgb = |buf: &[u8]| buf.iter().step_by(4).copied().collect::<Vec<u8>>();
    let once = rgb(&buf).iter().filter(|v| **v == 0xbf).count() as i32 == area
        && rgb(&buf).iter().all(|v| *v == 0x40 || *v == 0xbf);
    // SAFETY: as above; the previous canvas is dead, so access is exclusive.
    let mut canvas = unsafe { Canvas::new(buf.as_mut_ptr() as u64, w, h) };
    outline(&mut canvas, frame, clip);
    let twice = rgb(&buf).iter().all(|v| *v == 0x40);

    // Distinct outlines sharing most of an edge still invert each pixel once.
    let pair = [Rect::new(10, 10, 20, 8), Rect::new(12, 10, 20, 8)];
    let mut buffer = [Rect::new(0, 0, 0, 0); MAX_PIECES];
    let count = trail_pieces(&pair, &mut buffer);
    let pieces = &buffer[..count];
    let apart = pieces
        .iter()
        .enumerate()
        .all(|(i, a)| pieces[i + 1..].iter().all(|b| a.intersect(*b).is_empty()));
    buf.fill(0x40);
    // SAFETY: as above; the previous canvas is dead, so access is exclusive.
    let mut canvas = unsafe { Canvas::new(buf.as_mut_ptr() as u64, w, h) };
    for piece in pieces {
        canvas.invert(*piece, clip);
    }
    let in_trail = |x: i32, y: i32| {
        pair.iter().any(|r| {
            outline_strips(*r)
                .iter()
                .any(|s| x >= s.x && x < s.x + s.w && y >= s.y && y < s.y + s.h)
        })
    };
    let overlap_once = (0..h).all(|y| {
        (0..w).all(|x| {
            let v = buf[((y * w + x) * 4) as usize];
            v == if in_trail(x, y) { 0xbf } else { 0x40 }
        })
    });

    if ends
        && eased
        && timed
        && centred
        && clamped
        && disjoint
        && tiled
        && once
        && twice
        && apart
        && overlap_once
    {
        "XUID:ANIM:PASS\n"
    } else {
        "XUID:ANIM:FAIL\n"
    }
}
