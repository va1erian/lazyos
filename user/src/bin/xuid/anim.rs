//! Classic Mac-style window animation: a wireframe "zoom rectangle" that
//! flies between a window and its icon (taskbar entry) while the window
//! itself is not drawn. The compositor is single-threaded, so the animation
//! is a short blocking loop of a few frames, well under a quarter second.

use user::messenger::display::{Canvas, Color, Rect};
use user::sys;

use super::compositor::Compositor;
use super::layout::icon_rect;

/// Frames per phase (one PIT tick, 10 ms, each).
const STEPS: i32 = 10;
/// Outlines drawn per frame: the leading rectangle and two trailing ones.
const TRAIL: i32 = 3;
/// How far (in steps) each trailing outline lags the previous one.
const TRAIL_LAG: i32 = 1;
/// Outline thickness in pixels.
const LINE: i32 = 2;
const WIRE: Color = Color::rgb(236, 240, 250);

impl Compositor {
    /// Fly a wireframe from `from` to `to` over the screen as composed from
    /// the surfaces. The caller keeps the animated window out of the visible
    /// set (minimized) for the duration and repaints the full screen
    /// afterwards, which erases the last outline.
    fn zoom(&mut self, from: Rect, to: Rect) {
        let full = self.full();
        // The starting rectangle counts as previously drawn, so the first
        // frame also erases the window that was just hidden.
        let mut previous = from;
        for step in 1..=STEPS + TRAIL_LAG * (TRAIL - 1) {
            let deadline = sys::clock() + 1;
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
            self.compose(damage);
            for rect in rects.iter().filter(|rect| !rect.is_empty()) {
                outline(&mut self.screen, *rect, damage);
            }
            let _ = sys::display_present(damage.x, damage.y, damage.w, damage.h);
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
        let Some((window, icon, small)) = self.phases(id) else {
            return;
        };
        self.zoom(window, small);
        self.zoom(small, icon);
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
    /// the app hinted at, e.g. the folder tile just double-clicked) straight
    /// to the window, or from its taskbar entry when there is no hint.
    pub(super) fn open_zoom(&mut self, id: u64, origin: Option<Rect>) {
        let Some(from) = origin else {
            self.deiconify(id);
            return;
        };
        if let Some(surface) = self.surfaces.iter().find(|surface| surface.id == id) {
            let window = surface.window();
            self.zoom(from, window);
        }
    }

    /// Hide or show surface `id` without any other side effect.
    pub(super) fn set_minimized(&mut self, id: u64, minimized: bool) {
        if let Some(surface) = self.surfaces.iter_mut().find(|surface| surface.id == id) {
            surface.minimized = minimized;
        }
    }

    /// The window, its icon rectangle, and the icon-sized rectangle centred on
    /// the window that joins them.
    fn phases(&self, id: u64) -> Option<(Rect, Rect, Rect)> {
        let surface = self.surfaces.iter().find(|surface| surface.id == id)?;
        let window = surface.window();
        let icon = icon_rect(
            &self.surfaces,
            self.screen.width(),
            self.screen.height(),
            id,
        );
        let small = Rect::new(
            window.x + (window.w - icon.w) / 2,
            window.y + (window.h - icon.h) / 2,
            icon.w,
            icon.h,
        );
        Some((window, icon, small))
    }
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

/// Draw a hollow rectangle.
fn outline(screen: &mut Canvas, rect: Rect, clip: Rect) {
    let t = LINE.min(rect.w / 2).min(rect.h / 2).max(1);
    screen.fill(Rect::new(rect.x, rect.y, rect.w, t), clip, WIRE);
    screen.fill(
        Rect::new(rect.x, rect.y + rect.h - t, rect.w, t),
        clip,
        WIRE,
    );
    screen.fill(Rect::new(rect.x, rect.y, t, rect.h), clip, WIRE);
    screen.fill(
        Rect::new(rect.x + rect.w - t, rect.y, t, rect.h),
        clip,
        WIRE,
    );
}
