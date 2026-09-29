#![forbid(unsafe_code)]

//! The tool strip painter.

use xui_core::Theme;
use xui_core::backend::Canvas;
use xui_core::geometry::{Point, Rect};

use super::super::icons;
use super::{State, cell_px, item_rect};

/// Draws the strip: surface, separators, active/hover fills and glyphs.
pub(super) fn paint(canvas: &mut dyn Canvas, state: &State, theme: &Theme) {
    let bounds = canvas.bounds();
    let dpi = canvas.dpi();
    canvas.clear(theme.surface);
    canvas.draw_line(
        Point::new(bounds.left, bounds.bottom - 1),
        Point::new(bounds.right, bounds.bottom - 1),
        theme.border,
        1.0,
    );
    canvas.push_clip(Rect::new(
        bounds.left,
        bounds.top,
        bounds.right,
        bounds.bottom - 1,
    ));
    let mut previous_group = state.items.first().map(|item| item.group());
    for (index, item) in state.items.iter().enumerate() {
        let rect = item_rect(bounds, dpi, index);
        if rect.top >= bounds.bottom {
            break;
        }
        if rect.left + cell_px(dpi) > bounds.right {
            continue;
        }
        if previous_group != Some(item.group()) && previous_group.is_some() {
            canvas.draw_line(
                Point::new(rect.left, rect.top + 4),
                Point::new(rect.left, rect.bottom - 4),
                theme.border,
                1.0,
            );
        }
        previous_group = Some(item.group());
        let enabled = state.enabled(*item);
        if state.is_active(*item) {
            canvas.fill_rounded_rect(rect.shrink(2), 4.0, theme.selection);
        } else if state.pressed.get() == Some(index) {
            canvas.fill_rounded_rect(rect.shrink(2), 4.0, theme.pressed);
        } else if state.hover.get() == Some(index) {
            canvas.fill_rounded_rect(rect.shrink(2), 4.0, theme.hover);
        }
        let ink = if enabled {
            theme.text
        } else {
            theme.text_disabled
        };
        icons::draw(canvas, *item, rect, ink, dpi);
    }
    canvas.pop_clip();
}
