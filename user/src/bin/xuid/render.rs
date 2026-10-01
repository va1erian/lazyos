//! Frame composition and chrome painting (issue #194 split): the
//! occlusion-aware [`Compositor::repaint`] pass and the
//! desktop/window/taskbar/overlay drawing.

use user::messenger::display::{Canvas, Face, Rect};
use user::sys;

use super::compositor::Compositor;
use super::drag::draw_drag;
use super::icons;
use super::layout::for_each_entry;
use super::region::Region;
use super::shell::AltTab;
use super::surface::Surface;
use super::theme::{
    background, border_color, border_color_focus, empty_bg, empty_text, overlay_bg, overlay_border,
    overlay_selected, overlay_text, taskbar_bg, taskbar_entry, taskbar_entry_focus,
    taskbar_entry_min, text_on, title_bg, title_bg_focus, title_text, title_text_focus, window_bg,
    BUTTON, BUTTON_GAP, BUTTON_MARGIN, ENTRY_H, ENTRY_PAD, TASKBAR_H, TITLE_H,
};
use super::window::surface_by_id;

impl Compositor {
    /// Compose `damage` from the background, the desktop surface, every
    /// visible window in z-order, the fallback taskbar, the Alt+Tab overlay,
    /// the active drag & drop session (if any), and the cursor, then present
    /// exactly that rectangle.
    ///
    /// Only pixels a later layer would not overwrite are painted (issue #360):
    /// the taskbar, Alt+Tab panel, context menu and each window are opaque, so
    /// each layer draws only where nothing opaque lies above it inside the
    /// damage. The result is pixel-identical to painting every layer in full.
    pub(super) fn repaint(&mut self, damage: Rect) {
        // A window may hang off the screen, so damage rectangles derived from
        // its geometry can too; never compose or present off screen.
        let damage = damage.intersect(self.full());
        if damage.is_empty() {
            return;
        }
        self.compose(damage);
        let _ = sys::display_present(damage.x, damage.y, damage.w, damage.h);
    }

    /// [`Compositor::repaint`] without the present, so a caller can draw over
    /// the composed frame (the window-zoom wireframe) and present once.
    pub(super) fn compose(&mut self, damage: Rect) {
        let damage = damage.intersect(self.full());
        if damage.is_empty() {
            return;
        }
        let taskbar = self.taskbar();
        let screen = &mut self.screen;
        let surfaces = &self.surfaces;
        let focused = self.focused;
        let dims = (screen.width(), screen.height());
        let clock = self.clock.text();

        // Opaque layers above every window.
        let mut overlays = [Rect::new(0, 0, 0, 0); 3];
        let mut overlay_count = 0;
        let mut push_overlay = |rect: Rect| {
            overlays[overlay_count] = rect;
            overlay_count += 1;
        };
        if taskbar {
            push_overlay(taskbar_rect(dims));
        }
        if let Some(panel) = self
            .alt_tab
            .as_ref()
            .and_then(|tab| alt_tab_panel(dims, surfaces, tab))
        {
            push_overlay(panel);
        }
        if super::menu::is_open() {
            push_overlay(super::menu::rect(dims));
        }
        let overlays = &overlays[..overlay_count];
        let windows = || {
            surfaces
                .iter()
                .filter(|surface| !surface.desktop && !surface.minimized)
        };
        let desktop = surfaces.iter().find(|surface| surface.desktop);
        // The desktop hides the background only where it really has pixels.
        let desktop_area = desktop
            .filter(|surface| has_pixels(surface))
            .map(|surface| Rect::new(surface.x, surface.y, surface.w, surface.h));

        // Background: only where no desktop, window or overlay lies on top.
        let mut visible = Region::new(damage);
        subtract_all(&mut visible, overlays);
        for window in windows() {
            visible.subtract(window.window());
        }
        if let Some(area) = desktop_area {
            visible.subtract(area);
        }
        for piece in visible.rects() {
            screen.fill(*piece, *piece, background());
        }
        // The desktop paints above the background and below every window.
        if let Some(desktop) = desktop {
            let mut visible = Region::new(
                Rect::new(desktop.x, desktop.y, desktop.w, desktop.h).intersect(damage),
            );
            subtract_all(&mut visible, overlays);
            for window in windows() {
                visible.subtract(window.window());
            }
            for piece in visible.rects() {
                draw_desktop(screen, desktop, *piece);
            }
        }
        // Windows bottom-up, each clipped to what the windows and overlays
        // above it leave visible.
        for (index, surface) in windows().enumerate() {
            let mut visible = Region::new(surface.window().intersect(damage));
            subtract_all(&mut visible, overlays);
            for above in windows().skip(index + 1) {
                visible.subtract(above.window());
            }
            for piece in visible.rects() {
                draw_surface(screen, surface, focused == Some(surface.id), *piece);
            }
        }
        if taskbar {
            draw_taskbar(screen, surfaces, focused, clock, damage);
        }
        if let Some(session) = self.drag_session.as_ref() {
            draw_drag(screen, surfaces, session, self.pointer, damage);
        }
        if let Some(tab) = self.alt_tab.as_ref() {
            draw_alt_tab(screen, surfaces, tab, damage);
        }
        super::menu::draw(screen, damage);
        screen.cursor(self.pointer.0, self.pointer.1, damage);
    }
}

/// Remove every rectangle in `covers` from `region`.
fn subtract_all(region: &mut Region, covers: &[Rect]) {
    for cover in covers {
        region.subtract(*cover);
    }
}

/// The fallback taskbar's rectangle on a screen of `dims`.
fn taskbar_rect(dims: (i32, i32)) -> Rect {
    Rect::new(0, dims.1 - TASKBAR_H, dims.0, TASKBAR_H)
}

/// Whether the surface has a mapped buffer big enough for the dimensions it
/// was attached at. The window may since have been resized; the old buffer is
/// cropped or padded.
fn has_pixels(surface: &Surface) -> bool {
    surface.pixels != 0
        && surface.buf_w > 0
        && surface.buf_h > 0
        && surface.bytes >= (surface.buf_w as u64 * surface.buf_h as u64 * 4)
}

/// Blit the desktop surface's pixels across its rectangle; no chrome, no
/// fallback placeholder text (a desktop without pixels is just the background).
fn draw_desktop(screen: &mut Canvas, surface: &Surface, clip: Rect) {
    let area = Rect::new(surface.x, surface.y, surface.w, surface.h);
    if area.intersect(clip).is_empty() {
        return;
    }
    if has_pixels(surface) {
        // SAFETY: the mapping was installed by `display_map_buffer` for this
        // buffer; `bytes` is at least `buf_w * buf_h * 4` (see `has_pixels`),
        // so the slice describes exactly the source `blit` reads.
        let pixels = unsafe {
            core::slice::from_raw_parts(surface.pixels as *const u8, surface.bytes as usize)
        };
        screen.blit(pixels, surface.buf_w, surface.buf_h, area, clip);
    }
}

/// The Alt+Tab panel rectangle centered on a screen of `dims`, or `None` when
/// the cycle is empty (nothing is drawn).
fn alt_tab_panel(dims: (i32, i32), surfaces: &[Surface], tab: &AltTab) -> Option<Rect> {
    let (screen_w, screen_h) = dims;
    let rows = tab.order.len().min(12);
    if rows == 0 {
        return None;
    }
    let title_w = tab
        .order
        .iter()
        .filter_map(|id| surface_by_id(surfaces, *id))
        .map(|surface| Face::Sans.width(&surface.title))
        .max()
        .unwrap_or(0);
    let panel_w = (title_w + 48).clamp(180, (screen_w - 40).max(180));
    let panel_h = 26 + rows as i32 * ALT_TAB_ROW_H;
    Some(Rect::new(
        (screen_w - panel_w) / 2,
        (screen_h - panel_h) / 2,
        panel_w,
        panel_h,
    ))
}

/// Height of one Alt+Tab row.
const ALT_TAB_ROW_H: i32 = 18;

/// Draw the Alt+Tab overlay centered on the screen: one row per window in the
/// cycle, the selected row highlighted.
fn draw_alt_tab(screen: &mut Canvas, surfaces: &[Surface], tab: &AltTab, clip: Rect) {
    let dims = (screen.width(), screen.height());
    let Some(panel) = alt_tab_panel(dims, surfaces, tab) else {
        return;
    };
    let rows = tab.order.len().min(12);
    let row_h = ALT_TAB_ROW_H;
    if panel.intersect(clip).is_empty() {
        return;
    }
    screen.fill(panel, clip, overlay_bg());
    screen.fill(
        Rect::new(panel.x, panel.y, panel.w, 2),
        clip,
        overlay_border(),
    );
    screen.fill(
        Rect::new(panel.x, panel.y + panel.h - 2, panel.w, 2),
        clip,
        overlay_border(),
    );
    screen.fill(
        Rect::new(panel.x, panel.y, 2, panel.h),
        clip,
        overlay_border(),
    );
    screen.fill(
        Rect::new(panel.x + panel.w - 2, panel.y, 2, panel.h),
        clip,
        overlay_border(),
    );
    screen.text_face(
        panel.x + 10,
        panel.y + 4,
        "Alt+Tab",
        Face::Serif,
        overlay_text(),
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
            screen.fill(row, clip, overlay_selected());
        }
        if let Some(surface) = surface_by_id(surfaces, *id) {
            screen.text_face(
                row.x + 8,
                row.y + (row.h - Face::Sans.height()) / 2,
                &surface.title,
                Face::Sans,
                overlay_text(),
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
        border_color_focus()
    } else {
        border_color()
    };
    // Body, then a 1px frame and the title separator.
    screen.fill(window, clip, window_bg());
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
    let (title_fill, ink) = if focused {
        (title_bg_focus(), title_text_focus())
    } else {
        (title_bg(), title_text())
    };
    screen.fill(surface.title_bar(), clip, title_fill);
    screen.fill(
        Rect::new(window.x, surface.y + TITLE_H, window.w, 1),
        clip,
        border,
    );
    // The title stops before the button group on the right; a resizable
    // window has three buttons where a fixed-size one has two.
    let buttons = if surface.resizable() { 3 } else { 2 };
    let reserved = BUTTON * buttons + BUTTON_GAP * (buttons - 1) + BUTTON_MARGIN + 6;
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
        ink,
        title_clip,
    );
    // Close, maximize (resizable only) and minimize glyphs sit directly on the
    // title bar, in its text colour, so they match the chrome instead of
    // adding coloured tiles.
    icons::draw_close(screen, surface.close_button(), ink, clip);
    if surface.resizable() {
        let button = surface.maximize_button();
        if surface.maximized.is_some() {
            icons::draw_restore(screen, button, ink, clip);
        } else {
            icons::draw_maximize(screen, button, ink, clip);
        }
    }
    icons::draw_minimize(screen, surface.minimize_button(), ink, clip);

    // The app's pixels, or an explicit placeholder before AttachBuffer.
    let content = surface.content();
    if has_pixels(surface) {
        // The window may have grown since the buffer was attached: the strips
        // the (cropped) buffer does not cover show the window background.
        // Only those strips are filled, so a normal frame writes each pixel
        // once.
        let (cover_w, cover_h) = (surface.buf_w.min(content.w), surface.buf_h.min(content.h));
        let right = Rect::new(
            content.x + cover_w,
            content.y,
            content.w - cover_w,
            content.h,
        );
        let bottom = Rect::new(content.x, content.y + cover_h, cover_w, content.h - cover_h);
        for strip in [right, bottom]
            .into_iter()
            .filter(|strip| !strip.is_empty())
        {
            screen.fill(strip, clip, window_bg());
        }
        // SAFETY: the mapping was installed by `display_map_buffer` for this
        // buffer; `bytes` is at least `buf_w * buf_h * 4` (see `has_pixels`),
        // so the slice describes exactly the source `blit` reads.
        let pixels = unsafe {
            core::slice::from_raw_parts(surface.pixels as *const u8, surface.bytes as usize)
        };
        screen.blit(pixels, surface.buf_w, surface.buf_h, content, clip);
    } else {
        screen.fill(content, clip, empty_bg());
        screen.text_face(
            content.x + 10,
            content.y + 10,
            "Waiting for buffer...",
            Face::Serif,
            empty_text(),
            clip,
        );
    }
}

/// Draw the bottom taskbar: one entry per live surface in creation order, with
/// the focused entry highlighted and minimized ones dimmed.
fn draw_taskbar(
    screen: &mut Canvas,
    surfaces: &[Surface],
    focused: Option<u64>,
    clock: &str,
    clip: Rect,
) {
    let (screen_w, screen_h) = (screen.width(), screen.height());
    let bar = Rect::new(0, screen_h - TASKBAR_H, screen_w, TASKBAR_H);
    if bar.intersect(clip).is_empty() {
        return;
    }
    screen.fill(bar, clip, taskbar_bg());
    screen.fill(Rect::new(bar.x, bar.y, bar.w, 1), clip, border_color());
    for_each_entry(surfaces, screen_w, screen_h, |surface, rect| {
        let background = if focused == Some(surface.id) {
            taskbar_entry_focus()
        } else if surface.minimized {
            taskbar_entry_min()
        } else {
            taskbar_entry()
        };
        screen.fill(rect, clip, background);
        let accent = if focused == Some(surface.id) {
            border_color_focus()
        } else {
            border_color()
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
            text_on(background),
            rect.intersect(clip),
        );
    });
    // Date and time, right-aligned in the space `for_each_entry` reserved.
    let slot = super::clock::rect((screen_w, screen_h));
    let width = Face::Serif.width(clock);
    screen.text_face(
        slot.x + slot.w - width - super::clock::PAD,
        slot.y + (TASKBAR_H - Face::Serif.height()) / 2,
        clock,
        Face::Serif,
        text_on(taskbar_bg()),
        bar.intersect(clip),
    );
}
