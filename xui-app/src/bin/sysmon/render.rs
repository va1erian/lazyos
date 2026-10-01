//! Painting for the `sysmon` window: the header and tab strip, then the
//! Overview tab (the three memory gauges, the task table and the footer) or
//! the Services tab (`services_view.rs`). Kept in a sibling module of `sysmon.rs` so each
//! source file stays small; the state it reads lives in the parent module.

use xui_app::compact;
use xui_app::dashboard as dash;
use xui_app::format::{bytes, uptime};
use xui_app::sysinfo::{self, Snapshot, MAX_TASKS};
use xui_core::{Canvas, Color, Point, Rect, Theme};

use crate::services_view::{self, View, TABS_H};
use crate::{State, CARD_GAP, CARD_H, CARD_MIN_H, FOOTER_H, TABLE_GAP};

/// Paint the whole dashboard.
pub(super) fn paint(canvas: &mut dyn Canvas, state: &State) {
    let theme = Theme::light();
    let bounds = canvas.bounds();
    if compact::is_compact(bounds.width(), bounds.height()) {
        crate::compact_view::paint(canvas, theme, state);
        compact::paint_chip(canvas, theme, bounds);
        return;
    }
    let subtitle = format!(
        "{}×{} · 1 s refresh · [r] refresh  [c] compact  [q] quit",
        bounds.width(),
        bounds.height()
    );
    let framed = dash::frame(canvas, theme, "sysmon", &subtitle);
    services_view::paint_tabs(canvas, theme, bounds, state.view);
    let content = Rect::new(
        framed.left,
        framed.top + TABS_H,
        framed.right,
        framed.bottom,
    );
    if state.view == View::Services {
        services_view::paint(canvas, theme, content, state);
        compact::paint_chip(canvas, theme, bounds);
        return;
    }

    let Some(snapshot) = &state.snapshot else {
        paint_unavailable(canvas, theme, content, state.error.unwrap_or(-22));
        compact::paint_chip(canvas, theme, bounds);
        return;
    };

    // Budget the vertical space from the window height so a short client
    // window cannot let the footer overdraw the task rows or the cards
    // (issue #251).
    let cards_h = card_height(content);
    if cards_h > 0 {
        paint_memory(canvas, theme, content, cards_h, snapshot);
    }
    let table_top = content.top + if cards_h > 0 { cards_h + TABLE_GAP } else { 0 };
    paint_tasks(
        canvas,
        theme,
        content,
        table_top,
        content.bottom - FOOTER_H,
        snapshot,
    );
    paint_footer(canvas, theme, content, state, snapshot);
    compact::paint_chip(canvas, theme, bounds);
}

/// The memory-card height for `content`: between [`CARD_MIN_H`] and
/// [`CARD_H`] when the window fits the cards, a section heading, the table header, four
/// task rows and the footer; `0` otherwise, when the table instead uses the whole body.
/// Keeping both in the budget stops the sections overdrawing each other in a
/// short window (issue #251).
fn card_height(content: Rect) -> i32 {
    // Heading (28), the table header row and four task rows, the gap, footer.
    let reserved = 28 + dash::ROW * 5 + TABLE_GAP + FOOTER_H;
    match content.height() - reserved {
        room if room >= CARD_MIN_H => room.min(CARD_H),
        _ => 0,
    }
}

/// The three memory gauges.
fn paint_memory(
    canvas: &mut dyn Canvas,
    theme: Theme,
    content: Rect,
    card_height: i32,
    snapshot: &Snapshot,
) {
    let gap = CARD_GAP;
    let card_width = (content.width() - gap * 2) / 3;
    let cards = [
        Rect::new(
            content.left,
            content.top,
            content.left + card_width,
            content.top + card_height,
        ),
        Rect::new(
            content.left + card_width + gap,
            content.top,
            content.left + card_width * 2 + gap,
            content.top + card_height,
        ),
        Rect::new(
            content.left + card_width * 2 + gap * 2,
            content.top,
            content.right,
            content.top + card_height,
        ),
    ];

    paint_frames_card(canvas, theme, cards[0], snapshot);
    paint_slab_card(canvas, theme, cards[1], snapshot);
    paint_heap_card(canvas, theme, cards[2], snapshot);
}

/// `live / total` as a percentage of the total (`0.0` when the total is 0).
pub(crate) fn share(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 / total as f64
    }
}

/// The gauge colour: accent normally, warning close to full, danger at full.
pub(crate) fn gauge_color(theme: Theme, fraction: f64) -> Color {
    if fraction >= 0.98 {
        theme.danger
    } else if fraction >= 0.85 {
        theme.warning
    } else {
        theme.accent
    }
}

/// A card with a title, a gauge and three `key value` lines.
fn gauge_card(
    canvas: &mut dyn Canvas,
    theme: Theme,
    rect: Rect,
    title: &str,
    fraction: f64,
    lines: &[(&str, String)],
) {
    dash::card(canvas, theme, rect);
    canvas.draw_text(
        title,
        Rect::new(rect.left + 16, rect.top + 8, rect.right - 16, rect.top + 32),
        &dash::heading(theme.text, dash::SECTION),
    );
    let bar = Rect::new(
        rect.left + 16,
        rect.top + 42,
        rect.right - 16,
        rect.top + 56,
    );
    dash::bar(canvas, theme, bar, fraction, gauge_color(theme, fraction));
    for (index, (key, value)) in lines.iter().enumerate() {
        let top = rect.top + 64 + index as i32 * 24;
        dash::key_value(
            canvas,
            theme,
            Rect::new(rect.left + 16, top, rect.right - 16, top + 22),
            key,
            value,
            theme.text,
        );
    }
}

/// The physical-frame allocator card.
fn paint_frames_card(canvas: &mut dyn Canvas, theme: Theme, rect: Rect, snapshot: &Snapshot) {
    let fraction = share(snapshot.frames_live, snapshot.frames_total);
    let percent = (fraction * 100.0).round() as i64;
    gauge_card(
        canvas,
        theme,
        rect,
        "Frames",
        fraction,
        &[
            (
                "live",
                format!(
                    "{} / {} ({percent}%)",
                    snapshot.frames_live, snapshot.frames_total
                ),
            ),
            ("free", format!("{}", snapshot.frames_free)),
            ("reserved", format!("{}", snapshot.frames_reserved)),
            (
                "double frees",
                format!(
                    "{} · invalid {}",
                    snapshot.frames_double_frees, snapshot.frames_invalid_frees
                ),
            ),
        ],
    );
}

/// The slab allocator card.
fn paint_slab_card(canvas: &mut dyn Canvas, theme: Theme, rect: Rect, snapshot: &Snapshot) {
    let fraction = share(
        snapshot.slab_live,
        snapshot.slab_peak.max(snapshot.slab_live),
    );
    gauge_card(
        canvas,
        theme,
        rect,
        "Slab",
        fraction,
        &[
            ("live", bytes(snapshot.slab_live)),
            ("peak", bytes(snapshot.slab_peak)),
            (
                "oversized",
                format!(
                    "{} · peak {}",
                    bytes(snapshot.slab_oversized),
                    bytes(snapshot.slab_oversized_peak)
                ),
            ),
        ],
    );
}

/// The kernel heap card.
fn paint_heap_card(canvas: &mut dyn Canvas, theme: Theme, rect: Rect, snapshot: &Snapshot) {
    let fraction = share(snapshot.heap_used, snapshot.heap_total);
    let percent = (fraction * 100.0).round() as i64;
    gauge_card(
        canvas,
        theme,
        rect,
        "Kernel heap",
        fraction,
        &[
            (
                "used",
                format!(
                    "{} / {} ({percent}%)",
                    bytes(snapshot.heap_used),
                    bytes(snapshot.heap_total)
                ),
            ),
            ("free", bytes(snapshot.heap_free)),
            (
                "counters",
                format!(
                    "alloc {} · free {}",
                    snapshot.frames_allocated, snapshot.frames_freed
                ),
            ),
        ],
    );
}

/// The task table: pid, state, class, CPU ticks, name. Rows are limited to
/// what fits between `top` and `bottom` so the footer below never overlaps
/// them (issue #251).
fn paint_tasks(
    canvas: &mut dyn Canvas,
    theme: Theme,
    content: Rect,
    top: i32,
    bottom: i32,
    snapshot: &Snapshot,
) {
    if bottom <= top {
        return;
    }
    dash::section(
        canvas,
        theme,
        Rect::new(content.left, top, content.right, top + 24),
        &format!(
            "Tasks — {} live of {} slots (100 Hz ticks)",
            snapshot.tasks_live, MAX_TASKS
        ),
    );

    let header_top = top + 28;
    let header = dash::table_header_rect(content, header_top);
    draw_task_header(canvas, theme, header);

    let rows_top = header_top + dash::ROW;
    let capacity = ((bottom - rows_top) / dash::ROW).max(0) as usize;
    let live = snapshot.live_tasks().count();
    let shown = live.min(capacity);
    let mut y = rows_top;
    for row in snapshot.live_tasks().take(shown) {
        let rect = Rect::new(content.left, y, content.right, y + dash::ROW);
        let color = match row.state {
            sysinfo::TaskState::Runnable => theme.text,
            sysinfo::TaskState::Blocked => theme.text_secondary,
            _ => theme.text_disabled,
        };
        dash::cell(
            canvas,
            Rect::new(rect.left, y, rect.left + 70, rect.bottom),
            &format!("{}", row.pid),
            color,
            true,
        );
        dash::cell(
            canvas,
            Rect::new(rect.left + 80, y, rect.left + 170, rect.bottom),
            row.state.label(),
            color,
            false,
        );
        dash::cell(
            canvas,
            Rect::new(rect.left + 190, y, rect.left + 290, rect.bottom),
            row.class.label(),
            color,
            false,
        );
        dash::cell(
            canvas,
            Rect::new(rect.left + 310, y, rect.left + 420, rect.bottom),
            &format!("{} ({:.1}s)", row.cpu_ticks, row.cpu_ticks as f64 / 100.0),
            color,
            true,
        );
        dash::cell(
            canvas,
            Rect::new(rect.left + 430, y, rect.right, rect.bottom),
            &format!("{} (ppid {})", row.name(), row.ppid),
            color,
            false,
        );
        canvas.draw_line(
            Point::new(rect.left, rect.bottom),
            Point::new(rect.right, rect.bottom),
            theme.border,
            1.0,
        );
        y += dash::ROW;
    }
    if shown < live {
        canvas.draw_text(
            &format!("… {} more below", live - shown),
            Rect::new(content.left, top, content.right, top + 24),
            &dash::heading_end(theme.text_secondary, dash::SECTION),
        );
    }
}

/// The aligned task-table header.
fn draw_task_header(canvas: &mut dyn Canvas, theme: Theme, rect: Rect) {
    let columns: [(&str, i32, i32, bool); 5] = [
        ("pid", 0, 70, true),
        ("state", 80, 170, false),
        ("class", 190, 290, false),
        ("cpu", 310, 420, true),
        ("name", 430, rect.width(), false),
    ];
    for (label, left, right, end) in columns {
        let cell = Rect::new(rect.left + left, rect.top, rect.left + right, rect.bottom);
        let style = if end {
            dash::heading_end(theme.text_secondary, dash::LABEL)
        } else {
            dash::heading(theme.text_secondary, dash::LABEL)
        };
        canvas.draw_text(label, cell, &style);
    }
    canvas.draw_line(
        Point::new(rect.left, rect.bottom),
        Point::new(rect.right, rect.bottom),
        theme.border,
        1.0,
    );
}

/// The footer line: uptime, tick, refreshes and the snapshot status.
fn paint_footer(
    canvas: &mut dyn Canvas,
    theme: Theme,
    content: Rect,
    state: &State,
    snapshot: &Snapshot,
) {
    let footer = Rect::new(
        content.left,
        content.bottom - 26,
        content.right,
        content.bottom,
    );
    let status = "snapshot v2 via syscall 14";
    let text = format!(
        "uptime {} · tick {} · {} refresh(es) · {status}",
        uptime(snapshot.ticks),
        snapshot.ticks,
        state.refreshes
    );
    canvas.draw_text(
        &text,
        footer,
        &dash::heading(theme.text_secondary, dash::LABEL),
    );
    if let Some(code) = state.error {
        canvas.draw_text(
            &format!("last refresh failed: errno {code}"),
            footer,
            &dash::heading_end(theme.danger, dash::LABEL),
        );
    }
}

/// The error page, when no snapshot has ever decoded.
fn paint_unavailable(canvas: &mut dyn Canvas, theme: Theme, content: Rect, code: i64) {
    let card = Rect::new(content.left, content.top, content.right, content.top + 120);
    dash::card(canvas, theme, card);
    canvas.draw_text(
        "System snapshot unavailable",
        Rect::new(
            card.left + 16,
            card.top + 16,
            card.right - 16,
            card.top + 44,
        ),
        &dash::heading(theme.danger, dash::SECTION),
    );
    canvas.draw_text(
        &format!("syscall 14 returned errno {code}; the previous snapshot is kept once available"),
        Rect::new(
            card.left + 16,
            card.top + 48,
            card.right - 16,
            card.top + 96,
        ),
        &dash::heading(theme.text_secondary, dash::BODY),
    );
}
