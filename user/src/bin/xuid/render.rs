//! Frame composition and chrome painting (issue #194 split): the full-screen
//! [`repaint`] pass and the desktop/window/taskbar/overlay drawing, moved out
//! of `xuid.rs` unchanged.

use user::messenger::display::{Canvas, Face, Rect};
use user::sys;

use super::drag::{draw_drag, DragSession};
use super::layout::for_each_entry;
use super::shell::AltTab;
use super::surface::Surface;
use super::theme::{
    BACKGROUND, BORDER_COLOR, BORDER_COLOR_FOCUS, BUTTON, BUTTON_GAP, BUTTON_MARGIN, BUTTON_TEXT,
    CLOSE_BG, EMPTY_BG, ENTRY_H, ENTRY_PAD, MINIMIZE_BG, OVERLAY_BG, OVERLAY_BORDER,
    OVERLAY_SELECTED, OVERLAY_TEXT, TASKBAR_BG, TASKBAR_ENTRY, TASKBAR_ENTRY_FOCUS,
    TASKBAR_ENTRY_MIN, TASKBAR_H, TITLE_BG, TITLE_BG_FOCUS, TITLE_H, TITLE_TEXT, WINDOW_BG,
};
use super::window::surface_by_id;

/// Compose `damage` from the background, the desktop surface, every visible
/// window in z-order, the fallback taskbar, the Alt+Tab overlay, the active
/// drag & drop session (if any), and the cursor, then present exactly that
/// rectangle.
#[allow(clippy::too_many_arguments)]
pub(super) fn repaint(
    screen: &mut Canvas,
    surfaces: &[Surface],
    pointer: (i32, i32),
    focused: Option<u64>,
    damage: Rect,
    drag_session: Option<&DragSession>,
    taskbar: bool,
    alt_tab: Option<&AltTab>,
) {
    if damage.is_empty() {
        return;
    }
    compose(
        screen,
        surfaces,
        pointer,
        focused,
        damage,
        drag_session,
        taskbar,
        alt_tab,
    );
    let _ = sys::display_present(damage.x, damage.y, damage.w, damage.h);
}

/// [`repaint`] without the present, so a caller can draw over the composed
/// frame (the window-zoom wireframe) and present once.
#[allow(clippy::too_many_arguments)]
pub(super) fn compose(
    screen: &mut Canvas,
    surfaces: &[Surface],
    pointer: (i32, i32),
    focused: Option<u64>,
    damage: Rect,
    drag_session: Option<&DragSession>,
    taskbar: bool,
    alt_tab: Option<&AltTab>,
) {
    screen.fill(damage, damage, BACKGROUND);
    // The desktop paints above the background and below every window.
    if let Some(desktop) = surfaces.iter().find(|surface| surface.desktop) {
        draw_desktop(screen, desktop, damage);
    }
    for surface in surfaces
        .iter()
        .filter(|surface| !surface.desktop && !surface.minimized)
    {
        draw_surface(screen, surface, focused == Some(surface.id), damage);
    }
    if taskbar {
        draw_taskbar(screen, surfaces, focused, damage);
    }
    if let Some(session) = drag_session {
        draw_drag(screen, surfaces, session, pointer, damage);
    }
    if let Some(tab) = alt_tab {
        draw_alt_tab(screen, surfaces, tab, damage);
    }
    super::menu::draw(screen, damage);
    screen.cursor(pointer.0, pointer.1, damage);
}

/// Blit the desktop surface's pixels across its rectangle; no chrome, no
/// fallback placeholder text (a desktop without pixels is just the background).
fn draw_desktop(screen: &mut Canvas, surface: &Surface, clip: Rect) {
    let area = Rect::new(surface.x, surface.y, surface.w, surface.h);
    if area.intersect(clip).is_empty() {
        return;
    }
    if surface.pixels != 0 && surface.bytes >= (surface.w * surface.h * 4) as u64 {
        // Safety: the mapping was installed by `display_map_buffer` for this
        // buffer and the surface's geometry describes it.
        let pixels = unsafe {
            core::slice::from_raw_parts(surface.pixels as *const u8, surface.bytes as usize)
        };
        screen.blit(pixels, surface.w, surface.h, area, clip);
    }
}

/// Draw the Alt+Tab overlay centered on the screen: one row per window in the
/// cycle, the selected row highlighted.
fn draw_alt_tab(screen: &mut Canvas, surfaces: &[Surface], tab: &AltTab, clip: Rect) {
    let (screen_w, screen_h) = (screen.width(), screen.height());
    let rows = tab.order.len().min(12);
    if rows == 0 {
        return;
    }
    let title_w = tab
        .order
        .iter()
        .filter_map(|id| surface_by_id(surfaces, *id))
        .map(|surface| Face::Sans.width(&surface.title))
        .max()
        .unwrap_or(0);
    let panel_w = (title_w + 48).clamp(180, (screen_w - 40).max(180));
    let row_h = 18;
    let panel_h = 26 + rows as i32 * row_h;
    let panel = Rect::new(
        (screen_w - panel_w) / 2,
        (screen_h - panel_h) / 2,
        panel_w,
        panel_h,
    );
    if panel.intersect(clip).is_empty() {
        return;
    }
    screen.fill(panel, clip, OVERLAY_BG);
    screen.fill(
        Rect::new(panel.x, panel.y, panel.w, 2),
        clip,
        OVERLAY_BORDER,
    );
    screen.fill(
        Rect::new(panel.x, panel.y + panel.h - 2, panel.w, 2),
        clip,
        OVERLAY_BORDER,
    );
    screen.fill(
        Rect::new(panel.x, panel.y, 2, panel.h),
        clip,
        OVERLAY_BORDER,
    );
    screen.fill(
        Rect::new(panel.x + panel.w - 2, panel.y, 2, panel.h),
        clip,
        OVERLAY_BORDER,
    );
    screen.text_face(
        panel.x + 10,
        panel.y + 4,
        "Alt+Tab",
        Face::Serif,
        OVERLAY_TEXT,
        clip,
    );
    // Highlight the selected row before its text, then paint the titles.
    for (index, id) in tab.order.iter().take(rows).enumerate() {
        let row = Rect::new(
            panel.x + 6,
            panel.y + 22 + index as i32 * row_h,
            panel.w - 12,
            row_h - 2,
        );
        if index == tab.selected {
            screen.fill(row, clip, OVERLAY_SELECTED);
        }
        if let Some(surface) = surface_by_id(surfaces, *id) {
            screen.text_face(
                row.x + 8,
                row.y + (row.h - Face::Sans.height()) / 2,
                &surface.title,
                Face::Sans,
                OVERLAY_TEXT,
                row.intersect(clip),
            );
        }
    }
}

/// Draw one decorated window, clipped to `clip`.
fn draw_surface(screen: &mut Canvas, surface: &Surface, focused: bool, clip: Rect) {
    let window = surface.window();
    if window.intersect(clip).is_empty() {
        return;
    }
    let border = if focused {
        BORDER_COLOR_FOCUS
    } else {
        BORDER_COLOR
    };
    // Body, then a 1px frame and the title separator.
    screen.fill(window, clip, WINDOW_BG);
    screen.fill(Rect::new(window.x, window.y, window.w, 1), clip, border);
    screen.fill(
        Rect::new(window.x, window.y + window.h - 1, window.w, 1),
        clip,
        border,
    );
    screen.fill(Rect::new(window.x, window.y, 1, window.h), clip, border);
    screen.fill(
        Rect::new(window.x + window.w - 1, window.y, 1, window.h),
        clip,
        border,
    );
    // Title bar.
    screen.fill(
        surface.title_bar(),
        clip,
        if focused { TITLE_BG_FOCUS } else { TITLE_BG },
    );
    screen.fill(
        Rect::new(window.x, surface.y + TITLE_H, window.w, 1),
        clip,
        border,
    );
    // The title stops before the button group on the right.
    let reserved = BUTTON * 2 + BUTTON_GAP + BUTTON_MARGIN + 6;
    let title_clip = Rect::new(
        window.x + 2,
        surface.y,
        (window.w - 2 - reserved).max(0),
        TITLE_H,
    )
    .intersect(clip);
    screen.text_face(
        surface.x + 8,
        surface.y + (TITLE_H - Face::Sans.height()) / 2,
        &surface.title,
        Face::Sans,
        TITLE_TEXT,
        title_clip,
    );
    // Close and minimize buttons, painted over the title bar.
    for (rect, background, glyph) in [
        (surface.close_button(), CLOSE_BG, "X"),
        (surface.minimize_button(), MINIMIZE_BG, "-"),
    ] {
        screen.fill(rect, clip, background);
        let x = rect.x + (rect.w - Face::Sans.width(glyph)) / 2;
        let y = rect.y + (rect.h - Face::Sans.height()) / 2;
        screen.text_face(x, y, glyph, Face::Sans, BUTTON_TEXT, clip);
    }

    // The app's pixels, or an explicit placeholder before AttachBuffer.
    let content = surface.content();
    if surface.pixels != 0 && surface.bytes >= (surface.w * surface.h * 4) as u64 {
        // Safety: the mapping was installed by `display_map_buffer` for this
        // buffer and the surface's geometry describes it.
        let pixels = unsafe {
            core::slice::from_raw_parts(surface.pixels as *const u8, surface.bytes as usize)
        };
        screen.blit(pixels, surface.w, surface.h, content, clip);
    } else {
        screen.fill(content, clip, EMPTY_BG);
        screen.text_face(
            content.x + 10,
            content.y + 10,
            "Waiting for buffer...",
            Face::Serif,
            TITLE_TEXT,
            clip,
        );
    }
}

/// Draw the bottom taskbar: one entry per live surface in creation order, with
/// the focused entry highlighted and minimized ones dimmed.
fn draw_taskbar(screen: &mut Canvas, surfaces: &[Surface], focused: Option<u64>, clip: Rect) {
    let (screen_w, screen_h) = (screen.width(), screen.height());
    let bar = Rect::new(0, screen_h - TASKBAR_H, screen_w, TASKBAR_H);
    if bar.intersect(clip).is_empty() {
        return;
    }
    screen.fill(bar, clip, TASKBAR_BG);
    screen.fill(Rect::new(bar.x, bar.y, bar.w, 1), clip, BORDER_COLOR);
    for_each_entry(surfaces, screen_w, screen_h, |surface, rect| {
        let background = if focused == Some(surface.id) {
            TASKBAR_ENTRY_FOCUS
        } else if surface.minimized {
            TASKBAR_ENTRY_MIN
        } else {
            TASKBAR_ENTRY
        };
        screen.fill(rect, clip, background);
        let accent = if focused == Some(surface.id) {
            BORDER_COLOR_FOCUS
        } else {
            BORDER_COLOR
        };
        screen.fill(
            Rect::new(rect.x, rect.y + rect.h - 2, rect.w, 2),
            clip,
            accent,
        );
        screen.text_face(
            rect.x + ENTRY_PAD,
            rect.y + (ENTRY_H - Face::Sans.height()) / 2,
            &surface.title,
            Face::Sans,
            TITLE_TEXT,
            rect.intersect(clip),
        );
    });
}
