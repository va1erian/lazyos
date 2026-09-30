#![forbid(unsafe_code)]

//! Device-pixel metrics and viewport arithmetic for the monospace grid.
//!
//! The advance and line height are measured once per font and DPI; every other
//! position is arithmetic. Keeping this here, with no backend
//! dependency, lets the hit-testing and scroll range be unit-tested.

use xui_core::backend::TextMetrics;
use xui_core::geometry::Rect;
use xui_core::units::Dip;

/// The grid's device-pixel metrics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    /// One character cell's width.
    pub advance: i32,
    /// One line's height.
    pub line_height: i32,
    /// The gutter's width, zero when it is hidden.
    pub gutter: i32,
}

/// The design padding either side of the gutter's line numbers.
const GUTTER_PADDING: Dip = Dip(6.0);
/// The design width of a scrollbar.
pub const SCROLLBAR: Dip = Dip(12.0);
/// The design gap between the gutter and the text, so the first column does not
/// touch the line numbers.
const TEXT_PADDING: Dip = Dip(4.0);

impl Metrics {
    /// The metrics for a measured glyph cell and a line count.
    pub fn new(measured: TextMetrics, line_count: usize, show_gutter: bool, dpi: u32) -> Metrics {
        // Round to the nearest pixel: the painter draws each character in its
        // own cell, so this only sets the spacing, and nearest is closest to
        // the font's real advance.
        let advance = ((measured.width + CELL_PROBE_LEN / 2) / CELL_PROBE_LEN).max(1);
        let line_height = measured.height.max(1);
        let gutter = if show_gutter {
            let digits = digit_count(line_count) as i32;
            GUTTER_PADDING.to_px(dpi).value() * 2 + advance * digits.max(3)
        } else {
            0
        };
        Metrics {
            advance,
            line_height,
            gutter,
        }
    }

    /// The x of display column `col` inside `text`, given the first visible
    /// column.
    pub fn x_of_col(&self, text: Rect, col: usize, first_col: usize) -> i32 {
        text.left + (col as i32 - first_col as i32) * self.advance
    }

    /// The y of line `line` inside `text`, given the first visible line.
    pub fn y_of_line(&self, text: Rect, line: usize, first_line: usize) -> i32 {
        text.top + (line as i32 - first_line as i32) * self.line_height
    }

    /// The display column at `x`, given the first visible column.
    pub fn col_at(&self, text: Rect, x: i32, first_col: usize) -> usize {
        let relative = (x - text.left).max(0);
        first_col + (relative / self.advance) as usize
    }

    /// The line at `y`, clamped to `0..line_count`, given the first visible
    /// line.
    pub fn line_at(&self, text: Rect, y: i32, first_line: usize, line_count: usize) -> usize {
        let relative = (y - text.top).max(0);
        let line = first_line + (relative / self.line_height) as usize;
        line.min(line_count.saturating_sub(1))
    }
}

/// How many characters the probe string measures.
pub const CELL_PROBE_LEN: i32 = 10;

/// The probe string whose width is divided per character.
pub const CELL_PROBE: &str = "MMMMMMMMMM";

/// The number of decimal digits in `value` (at least one).
pub fn digit_count(value: usize) -> usize {
    let mut digits = 1;
    let mut remaining = value;
    while remaining >= 10 {
        remaining /= 10;
        digits += 1;
    }
    digits
}

/// The visible rectangles of one editor viewport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewport {
    /// The whole node.
    pub bounds: Rect,
    /// The line-number gutter.
    pub gutter: Rect,
    /// The text area, inside the gutter and the scrollbars.
    pub text: Rect,
    /// The vertical scrollbar, when the content overflows vertically.
    pub vbar: Option<Rect>,
    /// The horizontal scrollbar, when the content overflows horizontally.
    pub hbar: Option<Rect>,
    /// The number of fully visible lines.
    pub visible_lines: usize,
    /// The number of fully visible display columns.
    pub visible_cols: usize,
    /// The grid metrics.
    pub metrics: Metrics,
}

impl Viewport {
    /// Splits `bounds` into gutter, text area and scrollbars.
    pub fn split(
        bounds: Rect,
        metrics: Metrics,
        line_count: usize,
        max_cols: usize,
        dpi: u32,
    ) -> Viewport {
        let bar = SCROLLBAR.to_px(dpi).value();
        let lead = metrics.gutter + TEXT_PADDING.to_px(dpi).value();
        let content_height = line_count.max(1) as i32 * metrics.line_height;
        let content_width = max_cols.max(1) as i32 * metrics.advance;
        // Each bar takes room from the other axis, so decide them together:
        // the vertical check is redone once the horizontal bar is known.
        let mut v_needed = content_height > bounds.height();
        let h_needed = content_width > bounds.width() - lead - if v_needed { bar } else { 0 };
        if h_needed && !v_needed {
            v_needed = content_height > bounds.height() - bar;
        }
        let vbar_w = if v_needed { bar } else { 0 };
        let hbar_h = if h_needed { bar } else { 0 };

        let gutter = Rect::new(
            bounds.left,
            bounds.top,
            bounds.left + metrics.gutter,
            bounds.bottom - hbar_h,
        );
        let text = Rect::new(
            bounds.left + lead,
            bounds.top,
            bounds.right - vbar_w,
            bounds.bottom - hbar_h,
        );
        let vbar = v_needed.then(|| {
            Rect::new(
                bounds.right - bar,
                bounds.top,
                bounds.right,
                bounds.bottom - hbar_h,
            )
        });
        let hbar = h_needed.then(|| {
            Rect::new(
                bounds.left + metrics.gutter,
                bounds.bottom - bar,
                bounds.right - vbar_w,
                bounds.bottom,
            )
        });

        let visible_lines = (text.height() / metrics.line_height).max(1) as usize;
        let visible_cols = (text.width() / metrics.advance).max(1) as usize;
        Viewport {
            bounds,
            gutter,
            text,
            vbar,
            hbar,
            visible_lines,
            visible_cols,
            metrics,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measured(width: i32, height: i32) -> TextMetrics {
        TextMetrics {
            width,
            height,
            ascent: height - 3,
            descent: 3,
        }
    }

    #[test]
    fn metrics_divide_the_probe_width() {
        let metrics = Metrics::new(measured(80, 16), 100, true, 96);
        assert_eq!(metrics.advance, 8);
        assert_eq!(metrics.line_height, 16);
        assert_eq!(metrics.gutter, 12 + 8 * 3);
    }

    #[test]
    fn a_zero_advance_still_advances() {
        let metrics = Metrics::new(measured(0, 16), 1, false, 96);
        assert_eq!(metrics.advance, 1);
        assert_eq!(metrics.gutter, 0);
    }

    #[test]
    fn digit_count_counts() {
        assert_eq!(digit_count(0), 1);
        assert_eq!(digit_count(9), 1);
        assert_eq!(digit_count(10), 2);
        assert_eq!(digit_count(5000), 4);
    }

    #[test]
    fn position_arithmetic_round_trips() {
        let metrics = Metrics::new(measured(80, 16), 100, false, 96);
        let text = Rect::new(0, 0, 400, 320);
        assert_eq!(metrics.x_of_col(text, 5, 2), 24);
        assert_eq!(metrics.col_at(text, 24, 2), 5);
        assert_eq!(metrics.y_of_line(text, 3, 1), 32);
        assert_eq!(metrics.line_at(text, 32, 1, 100), 3);
    }

    #[test]
    fn the_text_starts_a_little_after_the_gutter() {
        let metrics = Metrics::new(measured(80, 16), 5, true, 96);
        let viewport = Viewport::split(Rect::new(0, 0, 400, 200), metrics, 5, 20, 96);
        assert_eq!(viewport.gutter.right, metrics.gutter);
        assert_eq!(
            viewport.text.left,
            metrics.gutter + 4,
            "a 4px gap at 96dpi keeps the first column off the line numbers"
        );
    }

    #[test]
    fn a_full_viewport_has_no_scrollbars() {
        let metrics = Metrics::new(measured(80, 16), 5, true, 96);
        let viewport = Viewport::split(Rect::new(0, 0, 400, 200), metrics, 5, 20, 96);
        assert!(viewport.vbar.is_none());
        assert!(viewport.hbar.is_none());
        assert_eq!(viewport.visible_lines, 12);
    }

    #[test]
    fn a_horizontal_bar_that_hides_lines_brings_the_vertical_bar() {
        // 12 lines of 16 px fit in 200 px, but not in the 188 px left once a
        // long line adds the 12 px horizontal bar.
        let metrics = Metrics::new(measured(80, 16), 12, false, 96);
        let viewport = Viewport::split(Rect::new(0, 0, 400, 200), metrics, 12, 100, 96);
        assert!(viewport.hbar.is_some());
        assert!(viewport.vbar.is_some());
    }

    #[test]
    fn overflowing_content_gets_scrollbars() {
        let metrics = Metrics::new(measured(80, 16), 1000, true, 96);
        let viewport = Viewport::split(Rect::new(0, 0, 200, 100), metrics, 1000, 100, 96);
        assert!(viewport.vbar.is_some());
        assert!(viewport.hbar.is_some());
        assert!(viewport.text.right <= viewport.bounds.right - 12);
        assert!(viewport.text.bottom <= viewport.bounds.bottom - 12);
    }
}
