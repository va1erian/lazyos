//! The vertical and horizontal scrollbars.

use xui_core::backend::Canvas;
use xui_core::theme::Theme;
use xui_core::widget::scrollbar::{self, Orientation, Scroll};

use crate::metrics::Viewport;
use crate::state::EditorState;

/// The vertical and horizontal scrollbars.
pub(super) fn paint_scrollbars(
    canvas: &mut dyn Canvas,
    state: &EditorState,
    viewport: &Viewport,
    first_line: usize,
    first_col: usize,
    xui_theme: &Theme,
) {
    let metrics = viewport.metrics;
    let line_count = state.buffer.line_count();
    if let Some(track) = viewport.vbar {
        let scroll = Scroll {
            viewport: viewport.text.height(),
            content: line_count as i32 * metrics.line_height,
            offset: first_line as i32 * metrics.line_height,
        };
        scrollbar::paint_state(
            canvas,
            track,
            scroll,
            Orientation::Vertical,
            *xui_theme,
            scrollbar::ThumbState::Normal,
        );
    }
    if let Some(track) = viewport.hbar {
        let scroll = Scroll {
            viewport: viewport.text.width(),
            content: state.buffer.max_line_cols(state.options.tab_width) as i32 * metrics.advance,
            offset: first_col as i32 * metrics.advance,
        };
        scrollbar::paint_state(
            canvas,
            track,
            scroll,
            Orientation::Horizontal,
            *xui_theme,
            scrollbar::ThumbState::Normal,
        );
    }
}
