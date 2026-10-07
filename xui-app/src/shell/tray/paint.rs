//! Painting the tray's cells on the taskbar: the picture (an outline in the
//! bar's ink, or an image), a hover highlight, the `Attention` pulse, the
//! badge, and the overflow chevron.

use lazyshell::tray::item::Status;
use lazyshell::tray::layout::icon_rect;
use lazyshell::tray::Tray;
use lazyshell::Rect as ShellRect;
use xui_core::backend::TextStyle;
use xui_core::{draw_icon, Canvas, Dip, Lucide, Rect, Rgba};

use super::super::ctx::Ctx;
use super::super::theme::color;
use super::icon::Drawn;

/// Ticks per half period of the attention pulse (two flashes a second).
pub const PULSE_TICKS: u64 = 50;
/// The badge's text size and height in design pixels.
const BADGE_TEXT: Dip = Dip(8.0);
const BADGE_H: i32 = 10;

/// Whether any item asks for attention (the bar then repaints to pulse).
pub fn any_attention(model: &Tray) -> bool {
    model
        .entries()
        .iter()
        .any(|entry| entry.status() == Status::Attention)
}

/// A design-pixel rectangle at scale `s`.
fn rect(r: ShellRect, s: i32) -> Rect {
    Rect::new(r.x * s, r.y * s, (r.x + r.w) * s, (r.y + r.h) * s)
}

/// Paint every cell. `hover` is the cell index under the pointer, `chevron`
/// whether the pointer is on the chevron.
pub fn paint(canvas: &mut dyn Canvas, ctx: &Ctx, hover: Option<usize>, chevron: bool) {
    let palette = ctx.theme.borrow().palette();
    let s = ctx.scale();
    let ink = uitheme::text_on(palette.taskbar_bg);
    let dpi = canvas.dpi();
    let tray = &ctx.tray;
    let layout = tray.layout.borrow();
    let model = tray.model.borrow();
    let pulse_on = (crate::sys::clock_ticks() / PULSE_TICKS).is_multiple_of(2);
    let highlight = Rgba::with_alpha(0xFF, 0xFF, 0xFF, 0x18);
    for (index, cell) in layout.cells.iter().enumerate() {
        let Some(entry) = model.get(&cell.app) else {
            continue;
        };
        let area = rect(cell.rect, s);
        if entry.status() == Status::Attention && pulse_on {
            let glow = uitheme::mix(palette.taskbar_entry_focus, palette.taskbar_bg, 1, 4);
            canvas.fill_rounded_rect(area.shrink(s), 4.0 * s as f32, color(glow));
        } else if hover == Some(index) {
            canvas.fill_rect_rgba(area.shrink(s), highlight);
        }
        let icon = rect(icon_rect(cell.rect), s);
        let dir = tray.icons_dir(&entry.app);
        match tray
            .pictures
            .borrow_mut()
            .resolve(entry, dir.as_deref(), s, ink)
        {
            Drawn::Outline(outline) => draw_icon(canvas, outline, icon, color(ink), dpi),
            Drawn::Image(image) => canvas.draw_image(&image, icon),
        }
        if let Some(badge) = entry.custom.as_ref().and_then(|item| item.badge.as_deref()) {
            paint_badge(canvas, area, s, badge, palette.taskbar_entry_focus);
        }
    }
    if let Some(chevron_rect) = layout.chevron {
        let area = rect(chevron_rect, s);
        if chevron {
            canvas.fill_rect_rgba(area.shrink(s), highlight);
        }
        let icon = rect(icon_rect(chevron_rect), s);
        draw_icon(canvas, Lucide::ChevronUp, icon, color(ink), dpi);
    }
}

/// A small pill with `text` at the top-right corner of the cell `area`.
fn paint_badge(canvas: &mut dyn Canvas, area: Rect, s: i32, text: &str, fill: u32) {
    let chars = text.chars().count() as i32;
    let w = (BADGE_H + (chars - 1).max(0) * 5) * s;
    let h = BADGE_H * s;
    let pill = Rect::new(area.right - w, area.top, area.right, area.top + h);
    canvas.fill_rounded_rect(pill, (h / 2) as f32, color(fill));
    let ink = color(uitheme::text_on(fill));
    let style = TextStyle::new(ink, BADGE_TEXT).middle().centered().bold();
    canvas.draw_text(text, pill, &style);
}
