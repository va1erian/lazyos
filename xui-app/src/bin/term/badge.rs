//! The size badge: a small `100 x 30` panel centred on the window while the
//! grid's size changes. The Terminal shows it for about a second after each
//! change; its timer runs only while it is up.

use lazyterm::{Palette, Rgb};
use xui_core::{Canvas, Color, Rect};

use super::look::Look;
use super::view::{intersect, overlaps, Layout};

/// How long the badge stays after the last size change.
pub const BADGE_MILLIS: u32 = 1000;

fn color(rgb: Rgb) -> Color {
    Color::rgb(rgb.0, rgb.1, rgb.2)
}

/// The badge's rectangle for `text`, centred in the layout's window: as wide
/// as the text in grid cells plus a cell of padding either side.
pub fn rect(layout: &Layout, text: &str) -> Rect {
    let width = (text.chars().count() as i32 + 2) * layout.cell_w;
    let height = layout.line_h * 3 / 2;
    let left = (layout.width - width) / 2;
    let top = (layout.height - height) / 2;
    Rect::new(left, top, left + width, top + height)
}

/// Draw the badge over everything else, inside `damage`.
pub fn paint(canvas: &mut dyn Canvas, palette: &Palette, look: &Look, layout: &Layout, text: &str, damage: Rect) {
    let panel = rect(layout, text);
    if !overlaps(panel, damage) {
        return;
    }
    let scale = look.metrics.scale;
    let fg = color(palette.fg());
    let edge = color(palette.cursor());
    canvas.fill_rounded_rect(panel, 6.0 * scale as f32, edge);
    let inner = intersect(Rect::new(panel.left + scale, panel.top + scale, panel.right - scale, panel.bottom - scale), panel);
    canvas.fill_rounded_rect(inner, 5.0 * scale as f32, color(palette.bg().lerp(palette.fg(), 0.16)));
    let at = Rect::new(
        panel.left + layout.cell_w,
        panel.top + (panel.height() - layout.line_h) / 2 + look.metrics.text_dy,
        panel.right,
        panel.bottom,
    );
    canvas.draw_text(text, at, &look.text_style(fg, false, false));
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAYOUT: Layout = Layout {
        width: 640,
        height: 400,
        pad: 10,
        line_h: 20,
        cell_w: 9,
        visible: 19,
    };

    #[test]
    fn the_badge_is_centred_and_as_wide_as_its_text() {
        let panel = rect(&LAYOUT, "100 x 30");
        assert_eq!(panel.width(), 10 * 9);
        assert_eq!((panel.left + panel.right) / 2, 320);
        assert_eq!((panel.top + panel.bottom) / 2, 200);
        assert!(rect(&LAYOUT, "80 x 24").width() < panel.width());
    }
}
