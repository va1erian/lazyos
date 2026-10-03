//! The compact `fabricmon` view: a title row and four headline rows (channels,
//! endpoints, queued messages, registry names). Sibling of `render.rs`, which
//! keeps the full panel.

use xui_app::compact;
use xui_app::dashboard as dash;
use xui_core::{Canvas, Rect, Theme};

use crate::State;

/// Height of the title row.
const TITLE_H: i32 = 28;
/// Height of one label/value row.
const ROW_H: i32 = 20;

/// Paint the compact view of `state` over the whole client area.
pub(super) fn paint(canvas: &mut dyn Canvas, theme: Theme, state: &State) {
    let bounds = xui_app::hidpi::design_bounds(canvas);
    canvas.clear(theme.background);
    let title = Rect::new(
        bounds.left + 12,
        bounds.top,
        bounds.right - 80,
        bounds.top + TITLE_H,
    );
    canvas.draw_text(
        "fabricmon",
        title,
        &dash::heading(theme.text, dash::SECTION),
    );
    compact::rule(canvas, theme.border, bounds, bounds.top + TITLE_H);

    let Some(stats) = &state.stats else {
        let text = format!(
            "fabric unavailable (errno {})",
            state.stats_error.unwrap_or(-22)
        );
        canvas.draw_text(
            &text,
            Rect::new(
                bounds.left + 12,
                bounds.top + TITLE_H + 4,
                bounds.right - 12,
                bounds.bottom,
            ),
            &dash::heading(theme.danger, dash::LABEL),
        );
        return;
    };
    let names = state.registry.as_ref().map_or(0, |entries| entries.len());
    let rows = [
        ("channels", stats.channels.to_string()),
        ("endpoints", stats.endpoints.to_string()),
        ("queued messages", stats.queued.to_string()),
        ("registry names", names.to_string()),
    ];
    for (index, (key, value)) in rows.iter().enumerate() {
        let top = bounds.top + TITLE_H + 6 + index as i32 * ROW_H;
        dash::key_value(
            canvas,
            theme,
            Rect::new(bounds.left + 12, top, bounds.right - 12, top + ROW_H),
            key,
            value,
            theme.text,
        );
    }
}
