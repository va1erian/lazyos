#![forbid(unsafe_code)]

//! The editor's painter: a monospace grid.
//!
//! One character cell's advance and one line's height are measured once per
//! paint; every other position is `col * advance`. Only the visible lines are
//! drawn, so a 5,000-line file costs the same as a short one. xui has no
//! caret-from-offset query, so the grid does its own arithmetic.

mod decor;
mod scrollbars;
mod text;

#[cfg(test)]
mod tests;

use xui_core::backend::Canvas;
use xui_core::geometry::Rect;
use xui_core::theme::Theme;

use crate::metrics::{CELL_PROBE, Metrics, Viewport};
use crate::state::EditorState;
use crate::theme::EditorTheme;

use decor::{
    paint_brackets, paint_caret, paint_current_line, paint_gutter, paint_marker_tints,
    paint_selection, paint_squiggles,
};
use scrollbars::paint_scrollbars;
use text::paint_lines;

/// Draws `state` into `canvas`.
///
/// xui's software compositor reports [`Canvas::bounds`] as the node's window
/// rectangle with no translation applied, so the painter moves the origin to
/// the node's corner and clips to it, then works in node-local coordinates, as
/// the mouse events do. On a canvas that honours the contract the bounds already
/// sit at the origin and this is a no-op.
pub(crate) fn paint(
    canvas: &mut dyn Canvas,
    state: &EditorState,
    theme: &EditorTheme,
    xui_theme: &Theme,
) {
    let origin = canvas.bounds();
    let area = Rect::from_size(origin.size());
    canvas.save();
    canvas.set_translation(origin.left as f32, origin.top as f32);
    canvas.push_clip(area);
    paint_local(canvas, state, theme, xui_theme, area);
    canvas.pop_clip();
    canvas.restore();
}

/// Draws `state` into `bounds`, the node's area at the canvas origin.
fn paint_local(
    canvas: &mut dyn Canvas,
    state: &EditorState,
    theme: &EditorTheme,
    xui_theme: &Theme,
    bounds: Rect,
) {
    let dpi = canvas.dpi();
    let style = state.options.font.style(theme.text);
    let measured = canvas.measure_text(CELL_PROBE, &style);
    let line_count = state.buffer.line_count();
    let metrics = Metrics::new(measured, line_count, state.options.show_gutter, dpi);
    let viewport = Viewport::split(
        bounds,
        metrics,
        line_count,
        state.buffer.max_line_cols(state.options.tab_width),
        dpi,
    );
    let first_line = state.view.first_line.min(line_count.saturating_sub(1));
    let last_line = (first_line + viewport.visible_lines).min(line_count);
    let first_col = state.view.first_col;

    canvas.fill_rect(bounds, theme.background);
    if state.options.show_gutter {
        canvas.fill_rect(viewport.gutter, theme.gutter_background);
    }

    paint_current_line(canvas, state, theme, &viewport, first_line, line_count);
    paint_marker_tints(canvas, state, theme, &viewport, first_line, last_line);

    if let Some((start, end)) = state.view.selection() {
        let color = if state.focused {
            theme.selection
        } else {
            theme.selection_unfocused
        };
        paint_selection(
            canvas, state, &viewport, first_line, last_line, start, end, color,
        );
    }

    paint_brackets(canvas, state, theme, &viewport, first_line, last_line);
    paint_lines(canvas, state, theme, &viewport, first_line, last_line);
    paint_squiggles(canvas, state, theme, &viewport, first_line, last_line);
    paint_gutter(canvas, state, theme, &viewport, first_line, last_line);
    paint_caret(canvas, state, theme, &viewport, first_line, last_line);

    paint_scrollbars(canvas, state, &viewport, first_line, first_col, xui_theme);

    let border = if state.focused {
        theme.border_focused
    } else {
        theme.border
    };
    canvas.stroke_rect(bounds, border, 1.0);
    if state.selected {
        canvas.stroke_rect(bounds, xui_theme.accent, 2.0);
    }
}
