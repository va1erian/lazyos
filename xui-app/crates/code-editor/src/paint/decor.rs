//! Decorative layers of the grid: the current line, selection, bracket match,
//! marker tints and squiggles, the gutter and the caret.

use xui_core::Color;
use xui_core::backend::{Canvas, TextAlign, TextVAlign};
use xui_core::geometry::{Point, Rect};

use crate::markers::MarkerKind;
use crate::metrics::Viewport;
use crate::state::EditorState;
use crate::text::display_col;
use crate::theme::EditorTheme;
use crate::view::caret_display_col;

/// The current line's highlight, behind the text.
pub(super) fn paint_current_line(
    canvas: &mut dyn Canvas,
    state: &EditorState,
    theme: &EditorTheme,
    viewport: &Viewport,
    first_line: usize,
    line_count: usize,
) {
    if !state.focused {
        return;
    }
    let line = state.buffer.line_of_char(state.view.caret);
    if line < first_line || line >= first_line + viewport.visible_lines || line >= line_count {
        return;
    }
    let y = viewport.metrics.y_of_line(viewport.text, line, first_line);
    let row = Rect::new(
        viewport.text.left,
        y,
        viewport.text.right,
        y + viewport.metrics.line_height,
    );
    canvas.fill_rect(row, theme.current_line);
}

/// Faint tints behind lines carrying error or warning markers.
pub(super) fn paint_marker_tints(
    canvas: &mut dyn Canvas,
    state: &EditorState,
    theme: &EditorTheme,
    viewport: &Viewport,
    first_line: usize,
    last_line: usize,
) {
    for marker in &state.markers {
        if marker.line < first_line || marker.line >= last_line {
            continue;
        }
        let color = match marker.kind {
            MarkerKind::Error => theme.error,
            MarkerKind::Warning => theme.warning,
            _ => continue,
        };
        let y = viewport
            .metrics
            .y_of_line(viewport.text, marker.line, first_line);
        let row = Rect::new(
            viewport.gutter.left,
            y,
            viewport.text.right,
            y + viewport.metrics.line_height,
        );
        canvas.fill_rect(row, color.lerp(theme.background, 0.85));
    }
}

/// The selection fill, clipped to the visible text area.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_selection(
    canvas: &mut dyn Canvas,
    state: &EditorState,
    viewport: &Viewport,
    first_line: usize,
    last_line: usize,
    start: usize,
    end: usize,
    color: Color,
) {
    let text = viewport.text;
    let metrics = viewport.metrics;
    let tab = state.options.tab_width;
    let first_col = state.view.first_col;
    let start_line = state.buffer.line_of_char(start);
    let end_line = state.buffer.line_of_char(end);
    canvas.push_clip(text);
    for line in start_line..=end_line {
        if line < first_line || line >= last_line {
            continue;
        }
        let line_start = state.buffer.line_start(line);
        let line_text = state.buffer.line_string(line);
        let line_len = line_text.chars().count();
        let from = if line == start_line {
            start.saturating_sub(line_start)
        } else {
            0
        };
        let to = if line == end_line {
            end.saturating_sub(line_start)
        } else {
            line_len
        };
        let x0 = metrics.x_of_col(text, display_col(&line_text, from, tab), first_col);
        let mut end_col = display_col(&line_text, to, tab);
        if to == line_len && line < end_line {
            // The newline cell is selected too.
            end_col += 1;
        }
        let mut x1 = metrics.x_of_col(text, end_col, first_col);
        if x1 <= x0 {
            x1 = x0 + metrics.advance;
        }
        let y = metrics.y_of_line(text, line, first_line);
        let cell = Rect::new(x0, y, x1, y + metrics.line_height);
        if let Some(cell) = intersect(cell, text) {
            canvas.fill_rect(cell, color);
        }
    }
    canvas.pop_clip();
}
/// A fill behind the bracket pair at the caret, if any.
pub(super) fn paint_brackets(
    canvas: &mut dyn Canvas,
    state: &EditorState,
    theme: &EditorTheme,
    viewport: &Viewport,
    first_line: usize,
    last_line: usize,
) {
    if !state.focused {
        return;
    }
    let Some((open, close)) = state
        .highlight
        .bracket_pair(&state.buffer, state.view.caret)
    else {
        return;
    };
    for position in [open, close] {
        let line = state.buffer.line_of_char(position);
        if line < first_line || line >= last_line {
            continue;
        }
        let start = state.buffer.line_start(line);
        let text = state.buffer.line_string(line);
        let col = display_col(&text, position - start, state.options.tab_width);
        let x = viewport
            .metrics
            .x_of_col(viewport.text, col, state.view.first_col);
        let y = viewport.metrics.y_of_line(viewport.text, line, first_line);
        let cell = Rect::new(
            x,
            y,
            x + viewport.metrics.advance,
            y + viewport.metrics.line_height,
        );
        if let Some(cell) = intersect(cell, viewport.text) {
            canvas.fill_rect(cell, theme.bracket_match);
        }
    }
}

/// A squiggle under each spanned marker.
pub(super) fn paint_squiggles(
    canvas: &mut dyn Canvas,
    state: &EditorState,
    theme: &EditorTheme,
    viewport: &Viewport,
    first_line: usize,
    last_line: usize,
) {
    let text = viewport.text;
    let metrics = viewport.metrics;
    let tab = state.options.tab_width;
    let first_col = state.view.first_col;
    canvas.push_clip(text);
    for marker in &state.markers {
        if !marker.has_span() || marker.line < first_line || marker.line >= last_line {
            continue;
        }
        let color = match marker.kind {
            MarkerKind::Error => theme.error,
            MarkerKind::Warning => theme.warning,
            _ => theme.gutter_text,
        };
        let line_text = state.buffer.line_string(marker.line);
        let start = display_col(&line_text, marker.start, tab);
        let end = display_col(&line_text, marker.end, tab).max(start + 1);
        let x0 = metrics.x_of_col(text, start, first_col);
        let x1 = metrics.x_of_col(text, end, first_col);
        let y = metrics.y_of_line(text, marker.line, first_line) + metrics.line_height - 2;
        if let Some(span) = intersect(Rect::new(x0, y, x1, y + 2), text) {
            canvas.draw_line(
                Point::new(span.left, span.top),
                Point::new(span.right, span.top),
                color,
                1.0,
            );
        }
    }
    canvas.pop_clip();
}

/// Line numbers and breakpoint dots.
pub(super) fn paint_gutter(
    canvas: &mut dyn Canvas,
    state: &EditorState,
    theme: &EditorTheme,
    viewport: &Viewport,
    first_line: usize,
    last_line: usize,
) {
    if !state.options.show_gutter {
        return;
    }
    let gutter = viewport.gutter;
    let metrics = viewport.metrics;
    let pad = 4;
    let mut number_style = state.options.font.style(theme.gutter_text);
    number_style.align = TextAlign::End;
    number_style.valign = TextVAlign::Middle;
    for line in first_line..last_line {
        let y = metrics.y_of_line(viewport.text, line, first_line);
        let row = Rect::new(
            gutter.left + pad,
            y,
            gutter.right - pad,
            y + metrics.line_height,
        );
        canvas.draw_text(&(line + 1).to_string(), row, &number_style);
    }
    for marker in &state.markers {
        if marker.kind != MarkerKind::Breakpoint
            || marker.line < first_line
            || marker.line >= last_line
        {
            continue;
        }
        let center = Point::new(
            gutter.left + pad,
            metrics.y_of_line(viewport.text, marker.line, first_line) + metrics.line_height / 2,
        );
        canvas.fill_ellipse(center, 3.0, 3.0, theme.breakpoint);
    }
}

/// The blinking caret.
pub(super) fn paint_caret(
    canvas: &mut dyn Canvas,
    state: &EditorState,
    theme: &EditorTheme,
    viewport: &Viewport,
    first_line: usize,
    last_line: usize,
) {
    if !state.focused || !state.blink_on {
        return;
    }
    let line = state.buffer.line_of_char(state.view.caret);
    if line < first_line || line >= last_line {
        return;
    }
    let col = caret_display_col(&state.buffer, state.options.tab_width, state.view.caret);
    let x = viewport
        .metrics
        .x_of_col(viewport.text, col, state.view.first_col);
    if x < viewport.text.left || x >= viewport.text.right {
        return;
    }
    let y = viewport.metrics.y_of_line(viewport.text, line, first_line);
    canvas.draw_line(
        Point::new(x, y),
        Point::new(x, y + viewport.metrics.line_height),
        theme.caret,
        1.0,
    );
}
/// The intersection of two rectangles, or `None` when they do not overlap.
fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let left = a.left.max(b.left);
    let top = a.top.max(b.top);
    let right = a.right.min(b.right);
    let bottom = a.bottom.min(b.bottom);
    (left < right && top < bottom).then(|| Rect::new(left, top, right, bottom))
}
