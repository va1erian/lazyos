//! Frame composition and chrome painting (issue #194 split): the
//! occlusion-aware [`Compositor::repaint`] pass and the desktop, window,
//! panel and overlay drawing. `xuid` paints no desktop UI of its own (issue
//! #157): the taskbar and menus are the shell's panels.

use user::messenger::display::{Canvas, Color, Face, Rect};
use user::sys;

use super::compositor::Compositor;
use super::drag::draw_drag;
use super::icons;
use super::region::Region;
use super::shell::AltTab;
use super::surface::Surface;
use super::theme::{
    background, border_color, border_color_focus, button, button_gap, button_margin, empty_bg,
    empty_text, overlay_bg, overlay_border, overlay_selected, overlay_text, px, title_bg,
    title_bg_focus, title_h, title_text, title_text_focus, window_bg,
};
use super::window::surface_by_id;

impl Compositor {
    /// Compose `damage` from the background, the desktop surface, every
    /// visible window in z-order, the shell's panels, the Alt+Tab overlay,
    /// the active drag & drop session (if any), and the cursor, then present
    /// exactly that rectangle.
    ///
    /// Only pixels a later layer would not overwrite are painted (issue #360):
    /// panels, the Alt+Tab panel and each window are opaque, so each layer
    /// draws only where nothing opaque lies above it inside the damage. The
    /// result is pixel-identical to painting every layer in full.
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
        let cursor = self.held.pointer(self.pointer);
        let screen = &mut self.screen;
        let surfaces = &self.surfaces;
        let focused = self.focused;
        let dims = (screen.width(), screen.height());

        let alt_tab = self
            .alt_tab
            .as_ref()
            .and_then(|tab| alt_tab_panel(dims, surfaces, tab))
            .unwrap_or(Rect::new(0, 0, 0, 0));
        let windows = || {
            surfaces
                .iter()
                .filter(|surface| surface.is_window() && !surface.minimized)
        };
        // Panels with pixels, in creation (paint) order; one without pixels
        // shows nothing and hides nothing.
        let panels = || {
            surfaces
                .iter()
                .filter(|surface| surface.is_panel() && has_pixels(surface))
        };
        // Everything opaque above the windows.
        let overlays = |region: &mut Region| {
            region.subtract(alt_tab);
            for panel in panels() {
                region.subtract(panel.window());
            }
        };
        let desktop = surfaces.iter().find(|surface| surface.is_desktop());
        // The desktop hides the background only where it really has pixels.
        let desktop_area = desktop
            .filter(|surface| has_pixels(surface))
            .map(Surface::window);

        // Background: only where no desktop, window or overlay lies on top.
        let mut visible = Region::new(damage);
        overlays(&mut visible);
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
            let mut visible = Region::new(desktop.window().intersect(damage));
            overlays(&mut visible);
            for window in windows() {
                visible.subtract(window.window());
            }
            for piece in visible.rects() {
                draw_chromeless(screen, desktop, *piece);
            }
        }
        // Windows bottom-up, each clipped to what the windows and overlays
        // above it leave visible.
        for (index, surface) in windows().enumerate() {
            let mut visible = Region::new(surface.window().intersect(damage));
            overlays(&mut visible);
            for above in windows().skip(index + 1) {
                visible.subtract(above.window());
            }
            for piece in visible.rects() {
                draw_surface(screen, surface, focused == Some(surface.id), *piece);
            }
        }
        // Panels above every window, in creation order, below Alt+Tab.
        for (index, panel) in panels().enumerate() {
            let mut visible = Region::new(panel.window().intersect(damage));
            visible.subtract(alt_tab);
            for above in panels().skip(index + 1) {
                visible.subtract(above.window());
            }
            for piece in visible.rects() {
                draw_chromeless(screen, panel, *piece);
            }
        }
        if let Some(session) = self.drag_session.as_ref() {
            draw_drag(screen, surfaces, session, cursor, damage);
        }
        if let Some(tab) = self.alt_tab.as_ref() {
            draw_alt_tab(screen, surfaces, tab, damage);
        }
        // The shutting-down screen covers everything, cursor included.
        if super::powerfeed::active() {
            super::powerfeed::draw(screen, damage);
            return;
        }
        screen.cursor_scaled(cursor.0, cursor.1, super::theme::scale(), damage);
    }
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

/// Blit a desktop or panel's pixels across its rectangle; no chrome, no
/// fallback placeholder (one without pixels just shows what is below).
fn draw_chromeless(screen: &mut Canvas, surface: &Surface, clip: Rect) {
    let area = surface.window();
    if area.intersect(clip).is_empty() || !has_pixels(surface) {
        return;
    }
    // SAFETY: the mapping was installed by `display_map_buffer` for this
    // buffer; `bytes` is at least `buf_w * buf_h * 4` (see `has_pixels`), so
    // the slice describes exactly the source `blit` reads.
    let pixels =
        unsafe { core::slice::from_raw_parts(surface.pixels as *const u8, surface.bytes as usize) };
    screen.blit(pixels, surface.buf_w, surface.buf_h, area, clip);
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
    let panel_w = (title_w + px(48)).clamp(px(180), (screen_w - px(40)).max(px(180)));
    let panel_h = px(26) + rows as i32 * px(ALT_TAB_ROW_H);
    Some(Rect::new(
        (screen_w - panel_w) / 2,
        (screen_h - panel_h) / 2,
        panel_w,
        panel_h,
    ))
}

/// Height of one Alt+Tab row, in design pixels.
const ALT_TAB_ROW_H: i32 = 18;

/// Draw the Alt+Tab overlay centered on the screen: one row per window in the
/// cycle, the selected row highlighted.
fn draw_alt_tab(screen: &mut Canvas, surfaces: &[Surface], tab: &AltTab, clip: Rect) {
    let dims = (screen.width(), screen.height());
    let Some(panel) = alt_tab_panel(dims, surfaces, tab) else {
        return;
    };
    let rows = tab.order.len().min(12);
    let row_h = px(ALT_TAB_ROW_H);
    let edge = px(2);
    if panel.intersect(clip).is_empty() {
        return;
    }
    screen.fill(panel, clip, overlay_bg());
    screen.fill(
        Rect::new(panel.x, panel.y, panel.w, edge),
        clip,
        overlay_border(),
    );
    screen.fill(
        Rect::new(panel.x, panel.y + panel.h - edge, panel.w, edge),
        clip,
        overlay_border(),
    );
    screen.fill(
        Rect::new(panel.x, panel.y, edge, panel.h),
        clip,
        overlay_border(),
    );
    screen.fill(
        Rect::new(panel.x + panel.w - edge, panel.y, edge, panel.h),
        clip,
        overlay_border(),
    );
    screen.text_face(
        panel.x + px(10),
        panel.y + px(4),
        "Alt+Tab",
        Face::Serif,
        overlay_text(),
        clip,
    );
    // Highlight the selected row before its text, then paint the titles.
    for (index, id) in tab.order.iter().take(rows).enumerate() {
        let row = Rect::new(
            panel.x + px(6),
            panel.y + px(22) + index as i32 * row_h,
            panel.w - px(12),
            row_h - px(2),
        );
        if index == tab.selected {
            screen.fill(row, clip, overlay_selected());
        }
        if let Some(surface) = surface_by_id(surfaces, *id) {
            screen.text_face(
                row.x + px(8),
                row.y + (row.h - Face::Sans.height()) / 2,
                &surface.title,
                Face::Sans,
                overlay_text(),
                row.intersect(clip),
            );
        }
    }
}

/// A title bar: a vertical gradient around `fill` (lighter at the top,
/// darker at the bottom), a highlight under the frame and a dark separator
/// above the content, framed by `border` on three sides; every line is
/// `line` pixels thick (one design pixel at the desktop's scale).
fn draw_title_bar(screen: &mut Canvas, bar: Rect, fill: Color, border: Color, line: i32, clip: Rect) {
    const WHITE: Color = Color::rgb(255, 255, 255);
    const BLACK: Color = Color::rgb(0, 0, 0);
    screen.fill_vgradient(bar, clip, fill.lerp(WHITE, 1, 7), fill.lerp(BLACK, 1, 6));
    screen.fill(
        Rect::new(bar.x + line, bar.y + line, bar.w - 2 * line, line),
        clip,
        fill.lerp(WHITE, 1, 4),
    );
    screen.fill(
        Rect::new(bar.x, bar.y + bar.h, bar.w, line),
        clip,
        fill.lerp(BLACK, 1, 2),
    );
    screen.fill(Rect::new(bar.x, bar.y, bar.w, line), clip, border);
    screen.fill(Rect::new(bar.x, bar.y, line, bar.h), clip, border);
    screen.fill(Rect::new(bar.x + bar.w - line, bar.y, line, bar.h), clip, border);
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
    // Body, then a one-design-pixel frame and the title separator.
    let line = px(1);
    screen.fill(window, clip, window_bg());
    screen.fill(Rect::new(window.x, window.y, window.w, line), clip, border);
    screen.fill(
        Rect::new(window.x, window.y + window.h - line, window.w, line),
        clip,
        border,
    );
    screen.fill(Rect::new(window.x, window.y, line, window.h), clip, border);
    screen.fill(
        Rect::new(window.x + window.w - line, window.y, line, window.h),
        clip,
        border,
    );
    // Title bar.
    let (title_fill, ink) = if focused {
        (title_bg_focus(), title_text_focus())
    } else {
        (title_bg(), title_text())
    };
    draw_title_bar(screen, surface.title_bar(), title_fill, border, line, clip);
    // The title stops before the button group on the right; a resizable
    // window has three buttons where a fixed-size one has two.
    let buttons = if surface.resizable() { 3 } else { 2 };
    let reserved = button() * buttons + button_gap() * (buttons - 1) + button_margin() + px(6);
    let title_clip = Rect::new(
        window.x + px(2),
        surface.y,
        (window.w - px(2) - reserved).max(0),
        title_h(),
    )
    .intersect(clip);
    screen.text_face(
        surface.x + px(8),
        surface.y + (title_h() - Face::Sans.height()) / 2,
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
            content.x + px(10),
            content.y + px(10),
            "Waiting for buffer...",
            Face::Serif,
            empty_text(),
            clip,
        );
    }
}
