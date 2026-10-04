//! Window placement (issue #194 split): the tiling grid inside the work area,
//! on-screen clamping, the cursor sprite rect and the fallback icon rectangle
//! the minimize zoom uses when the shell gave a window no icon geometry.

use user::messenger::display::Rect;

use super::surface::Surface;
use super::theme::{
    border, cascade_step, cascade_visible_w, icon_h, icon_w, pad, title_h, window_gap,
};

/// Place a new window of content size `width`×`height` inside the work area
/// `area` (issue #250; the shell's taskbar is outside it since issue #157).
///
/// Candidate cells form a grid of `columns`×`rows` sized to this window so
/// that cells never overlap; the window takes the first cell (left to right,
/// top to bottom) that no existing window covers. Occupancy comes from the
/// real window rectangles, not from a surface count, so a cell freed by a
/// closed window is reused and windows of different sizes cannot be covered.
/// When every cell is taken, placement cascades from the top-left by
/// [`cascade_step()`] per window, keeping the whole window (and so its
/// controls) inside the area when it fits, and only its title bar when it
/// cannot.
pub(super) fn place_window(
    area: Rect,
    surfaces: &[Surface],
    width: i32,
    height: i32,
) -> (i32, i32) {
    let win_w = width + border() * 2;
    let win_h = height + title_h() + border();
    let others = || {
        surfaces
            .iter()
            .filter(|surface| surface.is_window() && !surface.minimized)
    };
    let place = |x: i32, y: i32| {
        (
            area.x + clamp_on_screen(x, win_w, area.w, cascade_visible_w()),
            area.y + clamp_on_screen(y, win_h, area.h, title_h() + border()),
        )
    };

    let step_x = win_w + window_gap();
    let step_y = win_h + window_gap();
    let columns = (((area.w - pad() * 2).max(0) + window_gap()) / step_x).max(1);
    let rows = (((area.h - pad() * 2).max(0) + window_gap()) / step_y).max(1);

    for cell in 0..columns * rows {
        let (x, y) = place(
            pad() + (cell % columns) * step_x,
            pad() + (cell / columns) * step_y,
        );
        let candidate = Rect::new(x, y, win_w, win_h);
        if others().all(|surface| surface.window().intersect(candidate).is_empty()) {
            return (x, y);
        }
    }

    // Every cell is covered: cascade by how many windows are open, wrapping
    // once the offset would run off the area so it never sticks at one spot.
    let count = others().count() as i32;
    let steps = ((area.w.min(area.h) - pad() * 2) / cascade_step()).max(1);
    let step = (count % steps + 1) * cascade_step();
    place(pad() + step, pad() + step)
}

/// Clamp a window offset along one axis of size `extent` so the window stays
/// wholly inside `room`. A window that cannot fit keeps `visible` pixels of
/// itself (its title bar) reachable instead.
fn clamp_on_screen(origin: i32, extent: i32, room: i32, visible: i32) -> i32 {
    if extent <= room {
        origin.min(room - extent).max(0)
    } else {
        origin.min((room - visible.min(extent)).max(0))
    }
}

/// The rectangle the cursor sprite (10x10 design pixels, drawn at the UI
/// scale) occupies at `point`.
pub(super) fn cursor_rect(point: (i32, i32)) -> Rect {
    let scale = super::theme::scale();
    Rect::new(point.0 - scale, point.1 - scale, 11 * scale, 11 * scale)
}

/// Where surface `id` iconifies to: the taskbar entry the shell reported with
/// `SetIconGeometry`, or a small rectangle at the bottom-left of the screen.
pub(super) fn icon_rect(surfaces: &[Surface], screen_h: i32, id: u64) -> Rect {
    surfaces
        .iter()
        .find(|surface| surface.id == id)
        .and_then(|surface| surface.icon)
        .unwrap_or(Rect::new(4, screen_h - icon_h() - 4, icon_w(), icon_h()))
}
