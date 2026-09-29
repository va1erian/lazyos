//! Window placement and taskbar geometry (issue #194 split): the tiling grid,
//! on-screen clamping, the taskbar entry layout and the cursor sprite rect,
//! moved out of `xuid.rs` unchanged.

use user::messenger::display::{Face, Rect};

use super::surface::Surface;
use super::theme::{
    BORDER, CASCADE_STEP, CASCADE_VISIBLE_W, ENTRY_GAP, ENTRY_H, ENTRY_MARGIN, ENTRY_MIN_W,
    ENTRY_PAD, PAD, TASKBAR_H, TITLE_H, WINDOW_GAP,
};
use super::window::contains;

/// Place a new window of content size `width`×`height` (issue #250).
///
/// Candidate cells form a grid of `columns`×`rows` sized to this window so
/// that cells never overlap; the window takes the first cell (left to right,
/// top to bottom) that no existing window covers. Occupancy comes from the
/// real window rectangles, not from a surface count, so a cell freed by a
/// closed window is reused and windows of different sizes cannot be covered.
/// When every cell is taken, placement cascades from the top-left by
/// [`CASCADE_STEP`] per window, keeping the whole window (and so its
/// controls) on screen when it fits, and only its title bar when it cannot.
pub(super) fn place_window(
    screen: (i32, i32),
    surfaces: &[Surface],
    width: i32,
    height: i32,
) -> (i32, i32) {
    let win_w = width + BORDER * 2;
    let win_h = height + TITLE_H + BORDER;
    let others = || {
        surfaces
            .iter()
            .filter(|surface| !surface.desktop && !surface.minimized)
    };

    let area_w = (screen.0 - PAD * 2).max(0);
    let area_h = (screen.1 - PAD - TASKBAR_H - PAD).max(0);
    let step_x = win_w + WINDOW_GAP;
    let step_y = win_h + WINDOW_GAP;
    let columns = ((area_w + WINDOW_GAP) / step_x).max(1);
    let rows = ((area_h + WINDOW_GAP) / step_y).max(1);

    for cell in 0..columns * rows {
        let x = clamp_on_screen(
            PAD + (cell % columns) * step_x,
            win_w,
            screen.0,
            0,
            CASCADE_VISIBLE_W,
        );
        let y = clamp_on_screen(
            PAD + (cell / columns) * step_y,
            win_h,
            screen.1,
            TASKBAR_H,
            TITLE_H + BORDER,
        );
        let candidate = Rect::new(x, y, win_w, win_h);
        if others().all(|surface| surface.window().intersect(candidate).is_empty()) {
            return (x, y);
        }
    }

    // Every cell is covered: cascade by how many windows are open, wrapping
    // once the offset would run off the screen so it never sticks at one spot.
    let count = others().count() as i32;
    let steps = ((screen.0.min(screen.1) - PAD * 2) / CASCADE_STEP).max(1);
    let step = (count % steps + 1) * CASCADE_STEP;
    (
        clamp_on_screen(PAD + step, win_w, screen.0, 0, CASCADE_VISIBLE_W),
        clamp_on_screen(PAD + step, win_h, screen.1, TASKBAR_H, TITLE_H + BORDER),
    )
}

/// Clamp a window origin along one axis of size `extent` so the window stays
/// wholly inside `limit` (minus `reserved` for the taskbar). A window that
/// cannot fit keeps `visible` pixels of itself (its title bar) reachable
/// instead.
fn clamp_on_screen(origin: i32, extent: i32, limit: i32, reserved: i32, visible: i32) -> i32 {
    let room = limit - reserved;
    if extent <= room {
        origin.min(room - extent).max(0)
    } else {
        origin.min((room - visible.min(extent)).max(0))
    }
}
/// The width of a surface's taskbar entry: title width plus padding.
fn entry_width(surface: &Surface) -> i32 {
    (Face::Sans.width(&surface.title) + ENTRY_PAD * 2).max(ENTRY_MIN_W)
}

/// Visit every taskbar entry in stable creation (id) order, left to right.
///
/// Entries are as wide as their title, but never wider than an equal share of
/// the bar, so one long title cannot push later windows (minimized ones
/// included) off it; the title text is clipped to its entry. Entries stop only
/// when not even a minimum-width one fits.
pub(super) fn for_each_entry(
    surfaces: &[Surface],
    screen_w: i32,
    screen_h: i32,
    mut visit: impl FnMut(&Surface, Rect),
) {
    let count = surfaces.iter().filter(|surface| !surface.desktop).count() as i32;
    let usable = screen_w - ENTRY_MARGIN * 2 - ENTRY_GAP * (count - 1).max(0);
    let share = (usable / count.max(1)).max(ENTRY_MIN_W);
    let mut x = ENTRY_MARGIN;
    let mut last_id = 0u64;
    let y = screen_h - TASKBAR_H + (TASKBAR_H - ENTRY_H) / 2;
    while let Some(surface) = surfaces
        .iter()
        .filter(|surface| !surface.desktop && surface.id > last_id)
        .min_by_key(|surface| surface.id)
    {
        last_id = surface.id;
        let room = screen_w - ENTRY_MARGIN - x;
        if room < ENTRY_MIN_W {
            break;
        }
        let width = entry_width(surface).min(share).min(room);
        visit(surface, Rect::new(x, y, width, ENTRY_H));
        x += width + ENTRY_GAP;
    }
}

/// The taskbar entry under `point`, if any.
pub(super) fn taskbar_hit(
    surfaces: &[Surface],
    screen_w: i32,
    screen_h: i32,
    point: (i32, i32),
) -> Option<u64> {
    let mut hit = None;
    for_each_entry(surfaces, screen_w, screen_h, |surface, rect| {
        if hit.is_none() && contains(rect, point) {
            hit = Some(surface.id);
        }
    });
    hit
}
/// The 10x10 sprite rectangle the cursor occupies at `point`.
pub(super) fn cursor_rect(point: (i32, i32)) -> Rect {
    Rect::new(point.0 - 1, point.1 - 1, 11, 11)
}

/// Where surface `id` iconifies to: its taskbar entry, or a bottom-left slot
/// when the entry does not fit (or a shell hides the bar).
pub(super) fn icon_rect(surfaces: &[Surface], screen_w: i32, screen_h: i32, id: u64) -> Rect {
    let mut found = None;
    for_each_entry(surfaces, screen_w, screen_h, |surface, rect| {
        if surface.id == id {
            found = Some(rect);
        }
    });
    found.unwrap_or(Rect::new(
        ENTRY_MARGIN,
        screen_h - TASKBAR_H + (TASKBAR_H - ENTRY_H) / 2,
        ENTRY_MIN_W,
        ENTRY_H,
    ))
}
