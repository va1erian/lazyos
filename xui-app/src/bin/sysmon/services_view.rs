//! The `sysmon` tab strip and its Services tab (issue #489): one row per
//! supervised service (`init`'s phase, pid, restarts and dependencies) with
//! the health `healthd` retains for it. Sibling of `render.rs`, which keeps
//! the Overview tab.

use xui_app::dashboard as dash;
use xui_app::format::bytes;
use xui_app::services::{resident_bytes, Services, Tone};
use xui_core::backend::TextAlign;
use xui_core::{Canvas, Color, Point, Rect, TextStyle, Theme};

use crate::State;

/// Height the tab strip takes from the top of the content area.
pub(crate) const TABS_H: i32 = 36;
/// One tab's size.
const TAB_W: i32 = 132;
const TAB_TALL: i32 = 26;
/// The healthy-status colour (WinUI SystemFillColorSuccess); the theme only
/// carries warning and danger.
const GOOD: Color = Color::rgb(0x0f, 0x7b, 0x0f);

/// The two tabs of the full window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum View {
    Overview,
    Services,
}

impl View {
    const ALL: [View; 2] = [View::Overview, View::Services];

    fn label(self) -> &'static str {
        match self {
            View::Overview => "Overview [o]",
            View::Services => "Services [s]",
        }
    }

    /// The word the `SYSMON:VIEW:<name>` marker carries.
    pub(crate) fn marker(self) -> &'static str {
        match self {
            View::Overview => "overview",
            View::Services => "services",
        }
    }
}

/// Where tab `index` sits in a client area of `bounds`: just under the
/// header rule `dash::frame` draws.
fn tab_rect(bounds: Rect, index: usize) -> Rect {
    let left = bounds.left + dash::MARGIN + index as i32 * (TAB_W + 8);
    Rect::new(
        left,
        dash::CONTENT_TOP,
        left + TAB_W,
        dash::CONTENT_TOP + TAB_TALL,
    )
}

/// The tab a press at `(x, y)` hits, if any.
pub(crate) fn hit_tab(bounds: Rect, x: i32, y: i32) -> Option<View> {
    View::ALL.into_iter().enumerate().find_map(|(index, view)| {
        let rect = tab_rect(bounds, index);
        (x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom).then_some(view)
    })
}

/// Paint the strip with `active` highlighted.
pub(crate) fn paint_tabs(canvas: &mut dyn Canvas, theme: Theme, bounds: Rect, active: View) {
    for (index, view) in View::ALL.into_iter().enumerate() {
        let rect = tab_rect(bounds, index);
        let (fill, text) = if view == active {
            (theme.accent, theme.text_on_accent)
        } else {
            (theme.raised, theme.text_secondary)
        };
        canvas.fill_rounded_rect(rect, 6.0, fill);
        canvas.stroke_rounded_rect(rect, 6.0, theme.border, 1.0);
        let mut style = dash::heading(text, dash::LABEL);
        style.align = TextAlign::Center;
        canvas.draw_text(view.label(), rect, &style);
    }
}

fn tone_color(theme: Theme, tone: Tone) -> Color {
    match tone {
        Tone::Good => GOOD,
        Tone::Warning => theme.warning,
        Tone::Bad => theme.danger,
        Tone::Unknown => theme.text_secondary,
    }
}

/// The table columns: label, left, right (relative to the content) and
/// whether the cell is right-aligned. `detail` takes the rest of the width.
const COLUMNS: [(&str, i32, i32, bool); 8] = [
    ("service", 0, 130, false),
    ("state", 140, 230, false),
    ("pid", 230, 280, true),
    ("restarts", 290, 350, true),
    ("memory", 360, 440, true),
    ("health", 456, 526, false),
    ("depends on", 536, 640, false),
    ("detail", 650, i32::MAX, false),
];

fn column(rect: Rect, index: usize) -> Rect {
    let (_, left, right, _) = COLUMNS[index];
    let right = if right == i32::MAX {
        rect.right
    } else {
        (rect.left + right).min(rect.right)
    };
    Rect::new((rect.left + left).min(right), rect.top, right, rect.bottom)
}

/// Paint the Services tab into `content` (below the tab strip), starting at
/// row `state.scroll`, and record how many rows fit in `state.page`.
pub(crate) fn paint(canvas: &mut dyn Canvas, theme: Theme, content: Rect, state: &State) {
    let Some(view) = state.services.as_ref() else {
        notice(
            canvas,
            theme,
            content,
            "Loading services…",
            theme.text_secondary,
        );
        return;
    };
    let (good, warn, bad) = view.counts();
    let tasks = state
        .snapshot
        .as_ref()
        .map_or(&[][..], |snapshot| &snapshot.tasks[..]);
    let total: u64 = view
        .rows
        .iter()
        .filter_map(|row| resident_bytes(row.pid, tasks))
        .sum();
    let heading = format!(
        "Services — {} listed · {good} ok · {warn} degraded · {bad} down · {} resident",
        view.rows.len(),
        bytes(total)
    );
    dash::section(
        canvas,
        theme,
        Rect::new(content.left, content.top, content.right, content.top + 24),
        &heading,
    );
    if let Some(summary) = &view.summary {
        let color = tone_color(theme, Tone::of_health(&summary.status));
        canvas.draw_text(
            &format!("system: {} {}", summary.status, summary.detail),
            Rect::new(content.left, content.top, content.right, content.top + 24),
            &dash::heading_end(color, dash::LABEL),
        );
    }
    let mut top = content.top + 28;
    if let Some(text) = source_errors(view) {
        canvas.draw_text(
            &text,
            Rect::new(content.left, top, content.right, top + 20),
            &dash::heading(theme.danger, dash::LABEL),
        );
        top += 22;
    }
    if view.rows.is_empty() {
        notice(
            canvas,
            theme,
            Rect::new(content.left, top, content.right, content.bottom),
            "No supervised services (is the image built with LAZYOS_SERVICES=1?)",
            theme.text_secondary,
        );
        return;
    }
    paint_table(canvas, theme, content, top, view, state);
}

/// Which source failed, for the line under the heading.
fn source_errors(view: &Services) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(code) = view.init_error {
        parts.push(format!("init unavailable (errno {code})"));
    }
    if let Some(code) = view.health_error {
        parts.push(format!("healthd unavailable (errno {code})"));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// The header and as many rows as fit above `content.bottom`, from row
/// `state.scroll` on; when they do not all fit, the last line says which rows
/// are shown and how to scroll.
fn paint_table(
    canvas: &mut dyn Canvas,
    theme: Theme,
    content: Rect,
    top: i32,
    view: &Services,
    state: &State,
) {
    let header = dash::table_header_rect(content, top);
    for (index, (label, _, _, end)) in COLUMNS.iter().enumerate() {
        let style = if *end {
            dash::heading_end(theme.text_secondary, dash::LABEL)
        } else {
            dash::heading(theme.text_secondary, dash::LABEL)
        };
        canvas.draw_text(label, column(header, index), &style);
    }
    rule(canvas, theme, header);

    let rows_top = header.bottom;
    let mut capacity = ((content.bottom - rows_top) / dash::ROW).max(0) as usize;
    if view.rows.len() > capacity {
        // Keep the last line for the scroll note.
        capacity = capacity.saturating_sub(1);
    }
    state.page.set(capacity.max(1));
    // The offset was clamped against the previous page size; a resize can
    // change it, so clamp again for this paint.
    let first = xui_app::services::scroll(state.scroll, 0, view.rows.len(), capacity);
    let shown = (view.rows.len() - first).min(capacity);
    let tasks = state
        .snapshot
        .as_ref()
        .map_or(&[][..], |snapshot| &snapshot.tasks[..]);
    for (index, row) in view.rows.iter().skip(first).take(shown).enumerate() {
        let y = rows_top + index as i32 * dash::ROW;
        let rect = Rect::new(content.left, y, content.right, y + dash::ROW);
        let state_color = tone_color(theme, Tone::of_state(&row.state));
        let health_color = tone_color(theme, Tone::of_health(&row.health));
        let state = if row.state.is_empty() {
            "—"
        } else {
            &row.state
        };
        let memory = resident_bytes(row.pid, tasks).map_or(String::from("—"), bytes);
        let pid = if row.pid == 0 {
            String::from("—")
        } else {
            row.pid.to_string()
        };
        let cells: [(&str, Color, bool); 8] = [
            (&row.name, theme.text, false),
            (state, state_color, false),
            (&pid, theme.text, true),
            (&row.restarts.to_string(), theme.text, true),
            (&memory, theme.text, true),
            (&row.health, health_color, false),
            (&row.deps, theme.text_secondary, false),
            (&row.detail, theme.text_secondary, false),
        ];
        for (index, (text, color, end)) in cells.iter().enumerate() {
            let cell = column(rect, index);
            dash::cell(canvas, cell, &fit(text, cell.width()), *color, *end);
        }
        rule(canvas, theme, rect);
    }
    if shown < view.rows.len() {
        let note = format!(
            "rows {}–{} of {} · Up/Down, PgUp/PgDn, Home/End or the wheel to scroll",
            first + 1,
            first + shown,
            view.rows.len()
        );
        canvas.draw_text(
            &note,
            Rect::new(
                content.left,
                content.bottom - 20,
                content.right,
                content.bottom,
            ),
            &dash::heading_end(theme.text_secondary, dash::LABEL),
        );
    }
}

/// Widest average glyph of the body font, in pixels: a conservative bound
/// so a cut cell never runs into its neighbour (the canvas does not clip).
const GLYPH_W: i32 = 7;

/// `text` shortened with an ellipsis to what fits `width` pixels.
fn fit(text: &str, width: i32) -> String {
    let max = (width / GLYPH_W).max(1) as usize;
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max - 1).collect();
    out.push('…');
    out
}

fn rule(canvas: &mut dyn Canvas, theme: Theme, rect: Rect) {
    canvas.draw_line(
        Point::new(rect.left, rect.bottom),
        Point::new(rect.right, rect.bottom),
        theme.border,
        1.0,
    );
}

fn notice(canvas: &mut dyn Canvas, theme: Theme, rect: Rect, text: &str, color: Color) {
    let card = Rect::new(
        rect.left,
        rect.top,
        rect.right,
        (rect.top + 64).min(rect.bottom),
    );
    dash::card(canvas, theme, card);
    let style: TextStyle = dash::heading(color, dash::BODY);
    canvas.draw_text(
        text,
        Rect::new(card.left + 16, card.top, card.right - 16, card.bottom),
        &style,
    );
}
