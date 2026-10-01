//! Shared paint primitives for the windowed viewers: the page frame, cards,
//! section headings and horizontal gauges, all on semantic theme tokens.

use xui_core::backend::TextAlign;
use xui_core::{Canvas, Color, Dip, Point, Rect, TextStyle, Theme};

/// Title text size.
pub const TITLE: f32 = 23.0;
/// Subtitle/status text size.
pub const SUBTITLE: f32 = 13.0;
/// Section heading size.
pub const SECTION: f32 = 15.0;
/// Table and body text size.
pub const BODY: f32 = 13.0;
/// Label text size.
pub const LABEL: f32 = 12.0;
/// A row's height in the task/registry tables.
pub const ROW: i32 = 24;

/// The page margin.
pub const MARGIN: i32 = 24;

/// A style for body text.
pub fn style(color: Color, size: f32) -> TextStyle {
    TextStyle::new(color, Dip(size))
}

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

/// A body-text style aligned to the rectangle's right edge.
pub fn cell_end_style(color: Color) -> TextStyle {
    let mut style = TextStyle::new(color, Dip(BODY)).middle();
    style.align = TextAlign::End;
    style
}

/// The top of the content rectangle [`frame`] returns, below the header rule.
pub const CONTENT_TOP: i32 = 76;

/// Clears the page, paints the title/subtitle header and rule, and returns the
/// content rectangle (inside the margins).
pub fn frame(canvas: &mut dyn Canvas, theme: Theme, title: &str, subtitle: &str) -> Rect {
    let bounds = canvas.bounds();
    canvas.clear(theme.background);

    let header = Rect::new(MARGIN, 16, bounds.right - MARGIN, 52);
    canvas.draw_text(
        title,
        header,
        &TextStyle::new(theme.text, Dip(TITLE)).middle(),
    );
    if !subtitle.is_empty() {
        let subtitle_rect = Rect::new(
            bounds.left + bounds.width() / 2,
            16,
            bounds.right - MARGIN,
            52,
        );
        canvas.draw_text(
            subtitle,
            subtitle_rect,
            &heading_end(theme.text_secondary, SUBTITLE),
        );
    }
    canvas.draw_line(
        Point::new(bounds.left, 64),
        Point::new(bounds.right, 64),
        theme.border,
        1.0,
    );
    Rect::new(
        bounds.left + MARGIN,
        CONTENT_TOP,
        bounds.right - MARGIN,
        bounds.bottom - 16,
    )
}

/// A raised card with a border.
pub fn card(canvas: &mut dyn Canvas, theme: Theme, rect: Rect) {
    canvas.fill_rounded_rect(rect, 8.0, theme.raised);
    canvas.stroke_rounded_rect(rect, 8.0, theme.border, 1.0);
}

/// A section heading at the top-left of `rect`.
pub fn section(canvas: &mut dyn Canvas, theme: Theme, rect: Rect, text: &str) {
    canvas.draw_text(
        text,
        Rect::new(rect.left, rect.top, rect.right, rect.top + 24),
        &heading(theme.text, SECTION),
    );
}

/// A `key  value` line: the key in secondary text, the value right-aligned.
pub fn key_value(
    canvas: &mut dyn Canvas,
    theme: Theme,
    rect: Rect,
    key: &str,
    value: &str,
    value_color: Color,
) {
    canvas.draw_text(key, rect, &heading(theme.text_secondary, LABEL));
    canvas.draw_text(value, rect, &heading_end(value_color, LABEL));
}

/// A horizontal gauge: a rounded track filled to `fraction` of its width.
pub fn bar(canvas: &mut dyn Canvas, theme: Theme, rect: Rect, fraction: f64, color: Color) {
    let fraction = fraction.clamp(0.0, 1.0);
    canvas.fill_rounded_rect(rect, rect.height() as f32 / 2.0, theme.scrollbar_track);
    let filled = (rect.width() as f64 * fraction).round() as i32;
    if filled > 0 {
        let fill = Rect::new(rect.left, rect.top, rect.left + filled, rect.bottom);
        let radius = (fill.height() as f32 / 2.0).min(filled as f32);
        canvas.fill_rounded_rect(fill, radius, color);
    }
    canvas.stroke_rounded_rect(rect, rect.height() as f32 / 2.0, theme.border, 1.0);
}

/// A table header line in secondary text.
pub fn table_header(canvas: &mut dyn Canvas, theme: Theme, rect: Rect, text: &str) {
    canvas.draw_text(text, rect, &heading(theme.text_secondary, LABEL).bold());
    canvas.draw_line(
        Point::new(rect.left, rect.bottom),
        Point::new(rect.right, rect.bottom),
        theme.border,
        1.0,
    );
}

/// The rectangle a table header occupies: `ROW` tall, starting at `top`.
pub fn table_header_rect(content: Rect, top: i32) -> Rect {
    Rect::new(content.left, top, content.right, top + ROW)
}

/// Text vertically centred in `rect`, with an optional right alignment.
pub fn cell(canvas: &mut dyn Canvas, rect: Rect, text: &str, color: Color, align_end: bool) {
    let style = if align_end {
        cell_end_style(color)
    } else {
        TextStyle::new(color, Dip(BODY)).middle()
    };
    canvas.draw_text(text, rect, &style);
}
