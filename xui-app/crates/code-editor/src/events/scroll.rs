//! Wheel scrolling and scrollbar dragging: the vertical/horizontal scroll
//! positions a wheel notch or a scrollbar gesture produces.

use xui_core::app::Ui;
use xui_core::backend::WidgetId;
use xui_core::widget::scrollbar::{self, Orientation, Scroll};

use crate::metrics::Viewport;
use crate::state::{Drag, EditorState, Effect};

use super::{Outcome, viewport};

/// Rows a wheel notch scrolls.
pub(super) const WHEEL_ROWS: i32 = 3;
/// Columns a horizontal wheel notch scrolls.
pub(super) const WHEEL_COLS: i32 = 3;
/// The wheel delta of one notch. Both backends report `WHEEL_DELTA` units:
/// Win32 passes them through and the canvas backend scales winit's line
/// deltas by it (and passes a touchpad's pixel deltas as-is).
const WHEEL_NOTCH: i32 = 120;
/// Handles a wheel event, vertically or horizontally.
pub(super) fn wheel<M: 'static>(
    state: &mut EditorState,
    ui: &Ui<M>,
    id: WidgetId,
    delta: i16,
    horizontal: bool,
    shift: bool,
) -> Outcome {
    let layout = viewport(ui, id, state);
    if horizontal || shift {
        let max = max_first_col(state, &layout).max(0) as usize;
        state.view.first_col = wheel_scroll(
            state.view.first_col,
            &mut state.wheel_cols_rest,
            delta,
            WHEEL_COLS,
            max,
        );
    } else {
        let max = state
            .buffer
            .line_count()
            .saturating_sub(layout.visible_lines);
        state.view.first_line = wheel_scroll(
            state.view.first_line,
            &mut state.wheel_rows_rest,
            delta,
            WHEEL_ROWS,
            max,
        );
        state.view.goal_col = None;
    }
    Outcome::default()
}

/// The first visible line (or column) after a wheel `delta`: `per_notch`
/// steps per [`WHEEL_NOTCH`], proportionally for partial (touchpad) deltas,
/// clamped to `0..=max`.
///
/// Travel short of a whole step is kept in `rest` and added to the next
/// event, so a run of small deltas scrolls instead of rounding to nothing. A
/// positive delta is "away from the user", which shows earlier lines, so it
/// scrolls up.
pub(super) fn wheel_scroll(
    first: usize,
    rest: &mut i32,
    delta: i16,
    per_notch: i32,
    max: usize,
) -> usize {
    let units = *rest + i32::from(delta) * per_notch;
    *rest = units % WHEEL_NOTCH;
    let target = first as i64 - i64::from(units / WHEEL_NOTCH);
    let clamped = target.clamp(0, max as i64);
    // At an end, leftover travel pointing past it is dropped, so reversing
    // direction scrolls at once instead of first unwinding that travel.
    let outward = (clamped == 0 && *rest > 0) || (clamped == max as i64 && *rest < 0);
    if clamped != target || outward {
        *rest = 0;
    }
    clamped as usize
}
/// Starts a scrollbar drag or a page jump when `(x, y)` is on a bar, returning
/// whether it was handled.
pub(super) fn scrollbar_down<M: 'static>(
    state: &mut EditorState,
    ui: &Ui<M>,
    layout: &Viewport,
    first_line: usize,
    x: i32,
    y: i32,
) -> bool {
    let dpi = ui.dpi();
    if let Some(track) = layout.vbar
        && track.contains(xui_core::geometry::Point::new(x, y))
    {
        let scroll = vertical_scroll(state, layout, first_line);
        match scrollbar::thumb(track, scroll, Orientation::Vertical, dpi) {
            Some(thumb) if y >= thumb.top && y < thumb.bottom => {
                state.v_drag = Some(Drag {
                    start_offset: first_line as i32 * layout.metrics.line_height,
                    start_pointer: y,
                });
            }
            thumb => {
                let page = layout.visible_lines as i32;
                let above = thumb.is_some_and(|thumb| y < thumb.top);
                let target = if above {
                    first_line as i32 - page
                } else {
                    first_line as i32 + page
                };
                let max = state
                    .buffer
                    .line_count()
                    .saturating_sub(layout.visible_lines);
                state.view.first_line = target.clamp(0, max as i32) as usize;
            }
        }
        state.captured = true;
        state.effects.push(Effect::Capture);
        return true;
    }
    if let Some(track) = layout.hbar
        && track.contains(xui_core::geometry::Point::new(x, y))
    {
        let scroll = horizontal_scroll(state, layout);
        match scrollbar::thumb(track, scroll, Orientation::Horizontal, dpi) {
            Some(thumb) if x >= thumb.left && x < thumb.right => {
                state.h_drag = Some(Drag {
                    start_offset: state.view.first_col as i32 * layout.metrics.advance,
                    start_pointer: x,
                });
            }
            thumb => {
                let page = layout.visible_cols as i32;
                let before = thumb.is_some_and(|thumb| x < thumb.left);
                let target = if before {
                    state.view.first_col as i32 - page
                } else {
                    state.view.first_col as i32 + page
                };
                state.view.first_col = target.clamp(0, max_first_col(state, layout)) as usize;
            }
        }
        state.captured = true;
        state.effects.push(Effect::Capture);
        return true;
    }
    false
}

/// Applies an in-progress scrollbar drag, returning whether it is dragging.
pub(super) fn scrollbar_move<M: 'static>(
    state: &mut EditorState,
    ui: &Ui<M>,
    _id: WidgetId,
    layout: &Viewport,
    first_line: usize,
    x: i32,
    y: i32,
) -> bool {
    let dpi = ui.dpi();
    let metrics = layout.metrics;
    if let (Some(drag), Some(track)) = (state.v_drag, layout.vbar) {
        let scroll = vertical_scroll(state, layout, first_line);
        let pixels = scrollbar::offset_from_drag(
            track,
            scroll,
            Orientation::Vertical,
            drag.start_offset,
            drag.start_pointer,
            y,
            dpi,
        );
        let max = state
            .buffer
            .line_count()
            .saturating_sub(layout.visible_lines);
        state.view.first_line = ((pixels / metrics.line_height).max(0) as usize).min(max);
        return true;
    }
    if let (Some(drag), Some(track)) = (state.h_drag, layout.hbar) {
        let scroll = horizontal_scroll(state, layout);
        let pixels = scrollbar::offset_from_drag(
            track,
            scroll,
            Orientation::Horizontal,
            drag.start_offset,
            drag.start_pointer,
            x,
            dpi,
        );
        state.view.first_col = (pixels / metrics.advance).max(0) as usize;
        return true;
    }
    false
}

/// The vertical scroll state for `first_line`.
fn vertical_scroll(state: &EditorState, layout: &Viewport, first_line: usize) -> Scroll {
    Scroll {
        viewport: layout.text.height(),
        content: state.buffer.line_count() as i32 * layout.metrics.line_height,
        offset: first_line as i32 * layout.metrics.line_height,
    }
}

/// The horizontal scroll state.
/// The largest `first_col` that still shows text: the longest line's length
/// minus the visible columns.
pub(super) fn max_first_col(state: &EditorState, layout: &Viewport) -> i32 {
    state
        .buffer
        .max_line_cols(state.options.tab_width)
        .saturating_sub(layout.visible_cols) as i32
}

fn horizontal_scroll(state: &EditorState, layout: &Viewport) -> Scroll {
    Scroll {
        viewport: layout.text.width(),
        content: state.buffer.max_line_cols(state.options.tab_width) as i32
            * layout.metrics.advance,
        offset: state.view.first_col as i32 * layout.metrics.advance,
    }
}
