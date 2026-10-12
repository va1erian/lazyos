#![forbid(unsafe_code)]

//! The tool grid painter.

use xui_core::Theme;
use xui_core::backend::Canvas;
use xui_core::geometry::Point;
use xui_core::theme::look;

use super::super::icons;
use super::{State, item_rect};

/// Draws the grid: surface, a right-hand divider, active/hover fills and glyphs.
pub(super) fn paint(canvas: &mut dyn Canvas, state: &State, theme: &Theme) {
    let bounds = canvas.bounds();
    let dpi = canvas.dpi();
    look::band(canvas, theme);
    canvas.draw_line(
        Point::new(bounds.right - 1, bounds.top),
        Point::new(bounds.right - 1, bounds.bottom),
        theme.border,
        1.0,
    );
    for (index, tool) in state.items.iter().enumerate() {
        let rect = item_rect(bounds, dpi, index);
        if rect.top >= bounds.bottom {
            break;
        }
        if state.active.get() == *tool {
            canvas.fill_rounded_rect(rect.shrink(2), 4.0, theme.selection);
        } else if state.pressed.get() == Some(index) {
            canvas.fill_rounded_rect(rect.shrink(2), 4.0, theme.pressed);
        } else if state.hover.get() == Some(index) {
            canvas.fill_rounded_rect(rect.shrink(2), 4.0, theme.hover);
        }
        icons::draw(canvas, *tool, rect, theme.text, dpi);
    }
}
