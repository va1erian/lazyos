//! The compact `sysmon` view: a title row and three gauge rows (frames, slab,
//! heap), for a small window or a pinned-in-the-corner monitor. Sibling of
//! `render.rs`, which keeps the full dashboard.

use xui_core::theme::look;
use xui_app::compact;
use xui_app::dashboard as dash;
use xui_app::format::bytes;
use xui_core::{Canvas, Rect, Theme};

use crate::render::{gauge_color, share};
use crate::State;

/// Height of the title row.
const TITLE_H: i32 = 28;

/// Paint the compact view of `state` over the whole client area.
pub(super) fn paint(canvas: &mut dyn Canvas, theme: Theme, state: &State) {
    let bounds = xui_app::hidpi::design_bounds(canvas);
    look::paint_background(canvas, bounds, bounds, &theme);
    let title = Rect::new(
        bounds.left + 12,
        bounds.top,
        bounds.right - 80,
        bounds.top + TITLE_H,
    );
    canvas.draw_text("sysmon", title, &dash::heading(theme.text, dash::SECTION));
    compact::rule(canvas, theme.border, bounds, bounds.top + TITLE_H);

    let body = Rect::new(
        bounds.left + 12,
        bounds.top + TITLE_H + 4,
        bounds.right - 12,
        bounds.bottom - 4,
    );
    let Some(snapshot) = &state.snapshot else {
        let text = format!(
            "snapshot unavailable (errno {})",
            state.error.unwrap_or(-22)
        );
        canvas.draw_text(&text, body, &dash::heading(theme.danger, dash::LABEL));
        return;
    };
    let rows = [
        (
            "Frames",
            share(snapshot.frames_live, snapshot.frames_total),
            format!("{} / {}", snapshot.frames_live, snapshot.frames_total),
        ),
        (
            "Slab",
            share(
                snapshot.slab_live,
                snapshot.slab_peak.max(snapshot.slab_live),
            ),
            bytes(snapshot.slab_live),
        ),
        (
            "Heap",
            share(snapshot.heap_used, snapshot.heap_total),
            bytes(snapshot.heap_used),
        ),
    ];
    let row_h = (body.height() / rows.len() as i32).clamp(18, 30);
    for (index, (label, fraction, value)) in rows.iter().enumerate() {
        let top = body.top + index as i32 * row_h;
        let row = Rect::new(body.left, top, body.right, top + row_h);
        canvas.draw_text(
            label,
            Rect::new(row.left, row.top, row.left + 52, row.bottom),
            &dash::heading(theme.text_secondary, dash::LABEL),
        );
        let bar_top = row.top + (row_h - 10) / 2;
        let bar = Rect::new(
            row.left + 56,
            bar_top,
            (row.right - 96).max(row.left + 60),
            bar_top + 10,
        );
        dash::bar(canvas, theme, bar, *fraction, gauge_color(theme, *fraction));
        canvas.draw_text(
            value,
            Rect::new(bar.right + 6, row.top, row.right, row.bottom),
            &dash::heading_end(theme.text, dash::LABEL),
        );
    }
}
