//! Classic Mac-style window animation: a wireframe "zoom rectangle" that
//! flies between a window and its icon (taskbar entry) while the window
//! itself is not drawn. The compositor is single-threaded, so the animation
//! is a short blocking loop of a few frames, well under a quarter second.

use user::messenger::display::{Canvas, Color, Rect};
use user::sys;

use super::layout::icon_rect;
use super::render::compose;
use super::surface::Surface;

/// Frames per phase (one PIT tick, 10 ms, each).
const STEPS: i32 = 10;
/// Outlines drawn per frame: the leading rectangle and two trailing ones.
const TRAIL: i32 = 3;
/// How far (in steps) each trailing outline lags the previous one.
const TRAIL_LAG: i32 = 1;
/// Outline thickness in pixels.
const LINE: i32 = 2;
const WIRE: Color = Color::rgb(236, 240, 250);

/// Fly a wireframe from `from` to `to` over the screen as composed from
/// `surfaces`. The caller keeps the animated window out of `surfaces`' visible
/// set (minimized) for the duration and repaints the full screen afterwards,
/// which erases the last outline.
pub(super) fn zoom(
    screen: &mut Canvas,
    surfaces: &[Surface],
    pointer: (i32, i32),
    focused: Option<u64>,
    taskbar: bool,
    from: Rect,
    to: Rect,
) {
    let full = Rect::new(0, 0, screen.width(), screen.height());
    // The starting rectangle counts as previously drawn, so the first frame
    // also erases the window that was just hidden from `surfaces`.
    let mut previous = Some(from);
    for step in 1..=STEPS + TRAIL_LAG * (TRAIL - 1) {
        let deadline = sys::clock() + 1;
        let mut rects = [Rect::new(0, 0, 0, 0); TRAIL as usize];
        let mut damage = previous.unwrap_or(Rect::new(0, 0, 0, 0));
        for (index, slot) in rects.iter_mut().enumerate() {
            let at = (step - index as i32 * TRAIL_LAG).clamp(0, STEPS);
            if at == 0 {
                continue;
            }
            *slot = lerp(from, to, at);
            damage = if damage.is_empty() {
                *slot
            } else {
                damage.union(*slot)
            };
        }
        // One pixel of slack so the outlines' edges are always inside.
        let damage =
            Rect::new(damage.x - 1, damage.y - 1, damage.w + 2, damage.h + 2).intersect(full);
        compose(
            screen, surfaces, pointer, focused, damage, None, taskbar, None,
        );
        for rect in rects.iter().filter(|rect| !rect.is_empty()) {
            outline(screen, *rect, damage);
        }
        let _ = sys::display_present(damage.x, damage.y, damage.w, damage.h);
        previous = Some(damage);
        // Pace the frames: `wait` with no children just sleeps to the deadline.
        let _ = sys::wait(deadline);
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

/// Iconify in two phases: the window shrinks in place to a taskbar-entry
/// sized wireframe, which then slides to the entry. `id` must already be
/// marked minimized so the window is not composed underneath the wireframe.
pub(super) fn iconify(
    screen: &mut Canvas,
    surfaces: &[Surface],
    pointer: (i32, i32),
    focused: Option<u64>,
    taskbar: bool,
    id: u64,
) {
    let Some(surface) = surfaces.iter().find(|surface| surface.id == id) else {
        return;
    };
    let (window, icon, small) = phases(screen, surfaces, surface);
    zoom(screen, surfaces, pointer, focused, taskbar, window, small);
    zoom(screen, surfaces, pointer, focused, taskbar, small, icon);
}

/// The reverse of [`iconify`]: the wireframe slides from the entry to the
/// window's centre, then grows to the window, before it is shown.
pub(super) fn deiconify(
    screen: &mut Canvas,
    surfaces: &[Surface],
    pointer: (i32, i32),
    focused: Option<u64>,
    taskbar: bool,
    id: u64,
) {
    let Some(surface) = surfaces.iter().find(|surface| surface.id == id) else {
        return;
    };
    let (window, icon, small) = phases(screen, surfaces, surface);
    zoom(screen, surfaces, pointer, focused, taskbar, icon, small);
    zoom(screen, surfaces, pointer, focused, taskbar, small, window);
}

/// The window, its icon rectangle, and the icon-sized rectangle centred on
/// the window that joins them.
fn phases(screen: &Canvas, surfaces: &[Surface], surface: &Surface) -> (Rect, Rect, Rect) {
    let window = surface.window();
    let icon = icon_rect(surfaces, screen.width(), screen.height(), surface.id);
    let small = Rect::new(
        window.x + (window.w - icon.w) / 2,
        window.y + (window.h - icon.h) / 2,
        icon.w,
        icon.h,
    );
    (window, icon, small)
}
