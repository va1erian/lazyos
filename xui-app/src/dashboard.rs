//! Paint primitives the desktop widget (`widget.rs`) draws with: section and
//! label text sizes, headings and a horizontal gauge, on semantic theme tokens.

use xui_core::backend::TextAlign;
use xui_core::theme::look;
use xui_core::{Canvas, Color, Dip, Rect, TextStyle, Theme};

/// Section heading size.
pub const SECTION: f32 = 15.0;
/// Label text size.
pub const LABEL: f32 = 12.0;

/// A style for a section heading, vertically centred in its rectangle.
pub fn heading(color: Color, size: f32) -> TextStyle {
    TextStyle::new(color, Dip(size)).middle()
}

/// The same style, aligned to the rectangle's right edge.
pub fn heading_end(color: Color, size: f32) -> TextStyle {
    let mut style = heading(color, size);
    style.align = TextAlign::End;
    style
}

/// A horizontal gauge: a rounded track filled to `fraction` of its width.
pub fn bar(canvas: &mut dyn Canvas, theme: Theme, rect: Rect, fraction: f64, color: Color) {
    let fraction = fraction.clamp(0.0, 1.0);
    let groove = if look::decorated(&theme) {
        theme.input_background
    } else {
        theme.scrollbar_track
    };
    canvas.fill_rounded_rect(rect, rect.height() as f32 / 2.0, groove);
    let filled = (rect.width() as f64 * fraction).round() as i32;
    if filled > 0 {
        let fill = Rect::new(rect.left, rect.top, rect.left + filled, rect.bottom);
        let radius = (fill.height() as f32 / 2.0).min(filled as f32);
        look::face(canvas, fill, radius, color, &theme);
    }
    canvas.stroke_rounded_rect(rect, rect.height() as f32 / 2.0, theme.border, 1.0);
}
