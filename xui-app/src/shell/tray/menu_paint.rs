//! Painting a tray menu panel with the start menu's look: the overlay panel
//! colours, the hover face, greyed disabled rows, a check or a radio dot in a
//! left column, a separator line, a submenu arrow, and the Quit row.

use lazyshell::tray::item::MenuKind;
use lazyshell::tray::menu::{Kind, Shown};
use lazyshell::Rect as ShellRect;
use xui_core::backend::TextStyle;
use xui_core::theme::look;
use xui_core::{draw_icon, Canvas, Dip, Lucide, Point, Rect};

use super::super::ctx::Ctx;
use super::super::theme::{chrome_look, color, fill_bar};

/// Row text size (the start menu's).
const TEXT: Dip = Dip(12.0);
/// The left column a check or radio mark sits in, and the label after it.
const MARK_W: i32 = 22;
/// Room for the submenu arrow at a row's right end.
const ARROW_W: i32 = 16;

fn rect(r: ShellRect, s: i32) -> Rect {
    Rect::new(r.x * s, r.y * s, (r.x + r.w) * s, (r.y + r.h) * s)
}

/// Paint the panel at `depth` of the open tray menu.
pub fn paint(canvas: &mut dyn Canvas, ctx: &Ctx, depth: usize) {
    let palette = ctx.theme.borrow().palette();
    let s = ctx.scale();
    let deco = chrome_look(ctx.theme.borrow().is_dark());
    let bounds = canvas.bounds();
    fill_bar(canvas, bounds, palette.overlay_bg, &deco);
    canvas.stroke_rect(bounds, color(palette.overlay_border), s as f32);
    let menus = ctx.tray.menus.borrow();
    let Some(panel) = menus.get(depth) else {
        return;
    };
    let hover = panel.hover.get();
    for (index, row) in panel.rows.rows().iter().enumerate() {
        let Some(area) = panel.rows.row_rect(index).map(|r| rect(r, s)) else {
            continue;
        };
        if row.kind == Kind::Separator {
            let mid = (area.top + area.bottom) / 2;
            let line = Rect::new(area.left + 8 * s, mid, area.right - 8 * s, mid + s);
            canvas.fill_rect(line, color(palette.overlay_border));
            continue;
        }
        let lit = hover == Some(index) && row.enabled;
        if lit {
            let face = Rect::new(
                area.left + 2 * s,
                area.top + s,
                area.right - 2 * s,
                area.bottom - s,
            );
            look::face(
                canvas,
                face,
                4.0 * s as f32,
                color(palette.overlay_selected),
                &deco,
            );
        }
        let ink = if !row.enabled {
            uitheme::mix(palette.overlay_text, palette.overlay_bg, 3, 5)
        } else if lit {
            0xFF_FF_FF
        } else {
            palette.overlay_text
        };
        paint_row(canvas, area, s, row, color(ink));
    }
}

/// One row's mark, label and arrow in `area` (screen pixels).
fn paint_row(canvas: &mut dyn Canvas, area: Rect, s: i32, row: &Shown, ink: xui_core::Color) {
    let dpi = canvas.dpi();
    let mark = Rect::new(
        area.left + 4 * s,
        area.top,
        area.left + MARK_W * s,
        area.bottom,
    );
    match row.kind {
        Kind::Item {
            kind: MenuKind::Check,
            checked: true,
            ..
        } => {
            let icon = Rect::new(
                mark.left + 2 * s,
                mark.top + 4 * s,
                mark.left + 18 * s,
                mark.top + 20 * s,
            );
            draw_icon(canvas, Lucide::Check, icon, ink, dpi);
        }
        Kind::Item {
            kind: MenuKind::Radio,
            checked: true,
            ..
        } => {
            let centre = Point::new((mark.left + mark.right) / 2, (mark.top + mark.bottom) / 2);
            let r = 3.5 * s as f32;
            canvas.fill_ellipse(centre, r, r, ink);
        }
        _ => {}
    }
    let arrow = matches!(row.kind, Kind::Submenu { .. });
    let reserve = if arrow { ARROW_W } else { 4 };
    let label = Rect::new(
        area.left + MARK_W * s,
        area.top,
        area.right - reserve * s,
        area.bottom,
    );
    canvas.push_clip(label);
    let style = TextStyle::new(ink, TEXT).middle();
    let style = if row.kind == Kind::Quit {
        style.bold()
    } else {
        style
    };
    canvas.draw_text(&row.label, label, &style);
    canvas.pop_clip();
    if arrow {
        let icon = Rect::new(
            area.right - ARROW_W * s,
            area.top + 4 * s,
            area.right,
            area.top + 20 * s,
        );
        draw_icon(canvas, Lucide::ChevronRight, icon, ink, dpi);
    }
}
