//! The HUD of a shot being played: the swing meter, the line of text
//! about what just happened, and the card line.

use xui_core::backend::{Canvas, TextStyle};
use xui_core::geometry::Rect;
use xui_core::{Color, Dip};

use crate::game::Running;
use crate::hud::bevel_panel;
use crate::play::meter::{Meter, LATE_LIMIT};
use crate::play::Phase;

const MARGIN: i32 = 10;

/// Paints the meter and the message over the view.
pub fn paint(canvas: &mut dyn Canvas, run: &Running, bounds: Rect, s: i32) {
    if let Some((text, _)) = &run.play.message {
        toast(canvas, bounds, s, text);
    }
    if let Phase::Swing(meter) = &run.play.phase {
        swing(canvas, run, bounds, s, meter);
    }
}

fn text(canvas: &mut dyn Canvas, rect: Rect, label: &str, size: f32, bold: bool) {
    let mut style = TextStyle::new(Color::hex(0x101010), Dip(size));
    if bold {
        style = style.bold();
    }
    canvas.draw_text(label, rect, &style);
}

/// A line of text in a panel at the top of the view, centred.
fn toast(canvas: &mut dyn Canvas, bounds: Rect, s: i32, label: &str) {
    let width = (label.chars().count() as i32 * 9 + 40).clamp(200, 520) * s;
    let cx = (bounds.left + bounds.right) / 2;
    let panel = Rect::new(
        cx - width / 2,
        bounds.top + MARGIN * s + 80 * s,
        cx + width / 2,
        bounds.top + MARGIN * s + 112 * s,
    );
    bevel_panel(canvas, panel);
    let inner = Rect::new(
        panel.left + 10 * s,
        panel.top + 7 * s,
        panel.right - 6 * s,
        panel.bottom,
    );
    text(canvas, inner, label, 14.0, true);
}

/// Where along a bar `left..right` the meter value `v` sits: the bar runs
/// from just past the start line (`-LATE_LIMIT`) to a full swing.
fn at(left: i32, right: i32, v: f32) -> i32 {
    let k = (v + LATE_LIMIT) / (1.0 + LATE_LIMIT);
    left + ((right - left) as f32 * k.clamp(0.0, 1.0)) as i32
}

fn swing(canvas: &mut dyn Canvas, run: &Running, bounds: Rect, s: i32, meter: &Meter) {
    let play = &run.play;
    // Between the help panel and the overhead map where there is room.
    let free_left = bounds.left + (MARGIN + 330 + MARGIN) * s;
    let free_right = bounds.right - (MARGIN + 200 + MARGIN) * s;
    let (left, right) = if free_right - free_left >= 240 * s {
        (free_left, free_right)
    } else {
        (bounds.left + MARGIN * s, bounds.right - MARGIN * s)
    };
    let width = (right - left).min(440 * s).max(160 * s);
    let height = 52 * s;
    let cx = (left + right) / 2;
    let panel = Rect::new(
        cx - width / 2,
        bounds.bottom - MARGIN * s - height,
        cx + width / 2,
        bounds.bottom - MARGIN * s,
    );
    bevel_panel(canvas, panel);
    let title = Rect::new(
        panel.left + 8 * s,
        panel.top + 3 * s,
        panel.right - 8 * s,
        panel.top + 20 * s,
    );
    let prompt = match meter {
        Meter::Power { .. } => "click to set the power",
        Meter::Accuracy { .. } => "click on the line",
        _ => "",
    };
    text(
        canvas,
        title,
        &format!("{}   {prompt}", play.club().name),
        12.0,
        true,
    );

    let track = Rect::new(
        panel.left + 8 * s,
        panel.top + 22 * s,
        panel.right - 8 * s,
        panel.bottom - 8 * s,
    );
    canvas.fill_rect(track, Color::hex(0x202020));
    let (l, r) = (track.left, track.right);

    // The power so far, green to red.
    let filled = match meter {
        Meter::Power { cursor } => *cursor,
        Meter::Accuracy { power, .. } => *power,
        _ => 0.0,
    };
    let zero = at(l, r, 0.0);
    for (from, to, color) in [
        (0.0f32, 0.6f32, 0x30A030u32),
        (0.6, 0.85, 0xD0C020),
        (0.85, 1.0, 0xD03020),
    ] {
        let (a, b) = (at(l, r, from), at(l, r, to.min(filled)));
        if to.min(filled) > from {
            canvas.fill_rect(
                Rect::new(a, track.top + 2 * s, b, track.bottom - 2 * s),
                Color::hex(color),
            );
        }
    }
    // The start line, and the tick the shot was worked out for.
    let line = |canvas: &mut dyn Canvas, x: i32, color: u32, extra: i32| {
        canvas.fill_rect(
            Rect::new(
                x - s,
                track.top - extra * s,
                x + s,
                track.bottom + extra * s,
            ),
            Color::hex(color),
        );
    };
    line(canvas, zero, 0xFFFFFF, 0);
    line(canvas, at(l, r, play.swing_tick), 0x40E0FF, 3);
    // The cursor.
    let cursor = at(l, r, meter.cursor());
    let color = if matches!(meter, Meter::Accuracy { .. }) {
        0xFF4040
    } else {
        0xFFFFFF
    };
    canvas.fill_rect(
        Rect::new(
            cursor - s,
            track.top - 2 * s,
            cursor + 2 * s,
            track.bottom + 2 * s,
        ),
        Color::hex(color),
    );
    canvas.stroke_rect(track, Color::hex(0x808080), 1.0);
}
