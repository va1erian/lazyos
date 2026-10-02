//! Classic Mac-style window animation: an inverted (XOR) wireframe "zoom
//! rectangle" that flies between a window and its icon (taskbar entry) while
//! the window itself is not drawn. Every move is two-stepped: the window
//! shrinks to an icon-sized rectangle centred on it, which then travels to the
//! target. The compositor is single-threaded, so the animation is a short
//! blocking loop of a few frames, well under a quarter second per step.

use alloc::vec;
use alloc::vec::Vec;
use user::messenger::display::{Canvas, Rect};
use user::sys;

use super::compositor::Compositor;
use super::layout::{cursor_rect, icon_rect};
use super::region::Region;

/// Frames per phase (one PIT tick, 10 ms, each).
const STEPS: i32 = 10;
/// Outlines drawn per frame: the leading rectangle and two trailing ones.
const TRAIL: i32 = 3;
/// How far (in steps) each trailing outline lags the previous one.
const TRAIL_LAG: i32 = 1;
/// Outline thickness in pixels. Outlines are drawn inverted (XOR), so they
/// show on any background.
const LINE: i32 = 2;

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
        // The starting rectangle counts as previously drawn, so the first
        // frame also erases the window that was just hidden.
        let mut previous = from;
        let mut cursor = self.held.pointer(self.pointer);
        for step in 1..=STEPS + TRAIL_LAG * (TRAIL - 1) {
            let deadline = sys::clock() + 1;
            self.hold_pending_input();
            let moved_to = self.held.pointer(self.pointer);
            let pointer_damage = cursor_rect(cursor).union(cursor_rect(moved_to));
            cursor = moved_to;
            let mut rects = [Rect::new(0, 0, 0, 0); TRAIL as usize];
            let mut damage = previous;
            for (index, slot) in rects.iter_mut().enumerate() {
                let at = (step - index as i32 * TRAIL_LAG).clamp(0, STEPS);
                if at == 0 {
                    continue;
                }
                *slot = lerp(from, to, at);
                damage = damage.union(*slot);
            }
            // One pixel of slack so the outlines' edges are always inside.
            let damage =
                Rect::new(damage.x - 1, damage.y - 1, damage.w + 2, damage.h + 2).intersect(full);
            let pointer_damage = pointer_damage.intersect(full);
            self.compose(damage);
            self.compose(pointer_damage);
            // Overlapping outlines (equal ones as the eased motion settles, but
            // also distinct ones that share an edge) would cancel under XOR,
            // so the trail is drawn as disjoint pieces, each pixel inverted once.
            let mut pieces = [Rect::new(0, 0, 0, 0); MAX_PIECES];
            let count = trail_pieces(&rects, &mut pieces);
            for piece in &pieces[..count] {
                self.screen.invert(*piece, damage);
            }
            // The cursor stays above the outlines.
            for clip in [damage, pointer_damage] {
                self.screen.cursor(cursor.0, cursor.1, clip);
            }
            for shown in [damage, pointer_damage].iter().filter(|r| !r.is_empty()) {
                let _ = sys::display_present(shown.x, shown.y, shown.w, shown.h);
            }
            previous = damage;
            // Pace the frames: `wait` with no children just sleeps to the
            // deadline.
            let _ = sys::wait(deadline);
        }
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

/// The rectangle `at`/[`STEPS`] of the way from `from` to `to`, eased out so
/// the motion starts fast and settles like the classic zoom.
fn lerp(from: Rect, to: Rect, at: i32) -> Rect {
    let t = at * 256 / STEPS;
    let eased = 256 - (256 - t) * (256 - t) / 256;
    let mix = |a: i32, b: i32| a + (b - a) * eased / 256;
    Rect::new(
        mix(from.x, to.x),
        mix(from.y, to.y),
        mix(from.w, to.w).max(2),
        mix(from.h, to.h).max(2),
    )
}

/// The four disjoint strips of a hollow rectangle (top, bottom, then left and
/// right between them). They must not overlap: XOR drawing would cancel the
/// overlap, leaving gaps in the corners.
fn outline_strips(rect: Rect) -> [Rect; 4] {
    let t = LINE.min(rect.w / 2).min(rect.h / 2).max(1);
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
    let ends = lerp(from, to, STEPS) == to;
    // The eased motion starts past the linear midpoint.
    let mid = lerp(from, to, STEPS / 2);
    let eased = mid.x > (from.x + to.x) / 2 && mid.w > (from.w + to.w) / 2;

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
    let tiled = area == 30 * 20 - (30 - 2 * LINE) * (20 - 2 * LINE);

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
