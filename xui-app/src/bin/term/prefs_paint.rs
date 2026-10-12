//! Drawing the preferences panel (`prefs.rs`): the font list with a live
//! preview line, steppers, scheme swatches, cursor and bell choices, checkboxes
//! and text fields, in the colours of the scheme in force.

use lazyterm::palette::{scheme_by_name, NAMES};
use lazyterm::{Palette, Rgb};
use xui_core::{Canvas, Color, Dip, Rect, TextStyle};

use super::prefs::{layout, Ctl, Field, Part, Prefs, PrefsLayout};

fn color(rgb: Rgb) -> Color {
    Color::rgb(rgb.0, rgb.1, rgb.2)
}

/// Draw the panel over the window. `dark` picks the `desktop` scheme's
/// swatch.
pub fn paint(canvas: &mut dyn Canvas, prefs: &Prefs, window: (i32, i32), scale: i32, palette: &Palette, dark: bool, caret_on: bool) {
    let laid = layout(prefs, window, scale);
    let s = scale.max(1);
    let (bg, fg) = (palette.bg(), palette.fg());
    let edge = color(palette.cursor());
    canvas.fill_rect(laid.panel, edge);
    let inner = Rect::new(laid.panel.left + s, laid.panel.top + s, laid.panel.right - s, laid.panel.bottom - s);
    canvas.fill_rect(inner, color(bg.lerp(fg, 0.1)));
    let ui = |c: Rgb, size: f32| TextStyle::new(color(c), Dip(size));
    let (text, dim) = (ui(fg, 14.0), ui(fg.lerp(bg, 0.5), 13.0));
    let draw = |canvas: &mut dyn Canvas, s_: &str, x: i32, y: i32, style: &TextStyle| {
        canvas.draw_text(s_, Rect::new(x, y, x + 400 * s, y + 24 * s), style);
    };
    let (left, top) = (laid.panel.left, laid.panel.top);
    draw(canvas, "Preferences", left + 10 * s, top + 5 * s, &ui(fg, 15.0));
    if let Some(error) = prefs.error {
        draw(canvas, error, left + 130 * s, top + 6 * s, &ui(Rgb(235, 100, 100), 13.0));
    }
    // The font list and the preview under it.
    for (part, rect, _) in &laid.parts {
        if let Part::Family(name) = part {
            let current = *name == prefs.settings.family;
            if current {
                let tint = if prefs.focus == Ctl::Family { palette.cursor().lerp(bg, 0.45) } else { bg.lerp(fg, 0.25) };
                canvas.fill_rect(*rect, color(tint));
            }
            draw(canvas, name, rect.left + 6 * s, rect.top + 2 * s, &text);
        }
    }
    paint_preview(canvas, prefs, &laid, palette, s);
    // Right column labels.
    let x0 = left + 290 * s;
    let row = |i: i32| top + 34 * s + i * 26 * s;
    for (i, label) in [(0, "Size"), (1, "Spacing"), (2, "Colours"), (4, "Cursor"), (6, "Scrollback"), (7, "Bell"), (9, "Word chars"), (10, "Window size")] {
        draw(canvas, label, x0, row(i) + 2 * s, &dim);
    }
    for (part, rect, _) in &laid.parts {
        paint_part(canvas, prefs, part, *rect, palette, dark, caret_on, s, &text);
    }
}

fn paint_preview(canvas: &mut dyn Canvas, prefs: &Prefs, laid: &PrefsLayout, palette: &Palette, s: i32) {
    let r = laid.preview;
    canvas.fill_rect(r, color(palette.bg()));
    let style = TextStyle::new(color(palette.fg()), Dip(prefs.settings.size.min(24) as f32)).family(prefs.settings.family.clone());
    canvas.draw_text("AaBbCc 0123 {}[]~ iIlL1 oO0", Rect::new(r.left + 6 * s, r.top + 4 * s, r.right, r.bottom), &style);
}

#[allow(clippy::too_many_arguments)]
fn paint_part(canvas: &mut dyn Canvas, prefs: &Prefs, part: &Part, rect: Rect, palette: &Palette, dark: bool, caret_on: bool, s: i32, text: &TextStyle) {
    let (bg, fg) = (palette.bg(), palette.fg());
    let button = bg.lerp(fg, 0.3);
    let focus = color(palette.cursor());
    let ring = |canvas: &mut dyn Canvas, rect: Rect| {
        for r in [
            Rect::new(rect.left, rect.top, rect.right, rect.top + s),
            Rect::new(rect.left, rect.bottom - s, rect.right, rect.bottom),
            Rect::new(rect.left, rect.top, rect.left + s, rect.bottom),
            Rect::new(rect.right - s, rect.top, rect.right, rect.bottom),
        ] {
            canvas.fill_rect(r, focus);
        }
    };
    let label = |canvas: &mut dyn Canvas, rect: Rect, t: &str, dx: i32| {
        canvas.draw_text(t, Rect::new(rect.left + dx, rect.top + 3 * s, rect.right, rect.bottom), text);
    };
    let st = &prefs.settings;
    match part {
        Part::Family(_) => {}
        Part::Close => {
            canvas.fill_rect(rect, color(button));
            label(canvas, rect, "x", 8 * s);
        }
        Part::SizeStep(d) | Part::SpacingStep(d) => {
            canvas.fill_rect(rect, color(button));
            label(canvas, rect, if *d < 0 { "-" } else { "+" }, 9 * s);
            let (value, at) = match part {
                Part::SizeStep(_) => (format!("{}", st.size), 0),
                _ => (format!("{}%", st.line_spacing), 0),
            };
            if *d > 0 {
                let _ = at;
                let vr = Rect::new(rect.left - 70 * s, rect.top, rect.left - 4 * s, rect.bottom);
                canvas.draw_text(&value, Rect::new(vr.left + 8 * s, vr.top + 3 * s, vr.right, vr.bottom), text);
            }
            let ctl = if matches!(part, Part::SizeStep(_)) { Ctl::Size } else { Ctl::Spacing };
            if prefs.focus == ctl && *d < 0 {
                ring(canvas, rect);
            }
        }
        Part::Scheme(i) => {
            let name = NAMES[*i];
            let scheme = scheme_by_name(name, dark);
            canvas.fill_rect(rect, color(scheme.bg));
            for (k, c) in [1usize, 2, 4, 5, 6].iter().enumerate() {
                let x = rect.left + 4 * s + k as i32 * 9 * s;
                canvas.fill_rect(Rect::new(x, rect.top + 4 * s, x + 7 * s, rect.top + 11 * s), color(scheme.ansi[*c]));
            }
            let short = match name {
                "solarized-dark" => "sol dark",
                "solarized-light" => "sol light",
                "gruvbox-dark" => "gruvbox",
                other => other,
            };
            canvas.draw_text(short, Rect::new(rect.left + 3 * s, rect.top + 12 * s, rect.right, rect.bottom), &TextStyle::new(color(scheme.fg), Dip(9.0)));
            if st.scheme == name {
                ring(canvas, rect);
            } else if prefs.focus == Ctl::Scheme {
                canvas.fill_rect(Rect::new(rect.left, rect.bottom, rect.right, rect.bottom + s), focus);
            }
        }
        Part::Cursor(i) | Part::Bell(i) => {
            let (on, name) = match part {
                Part::Cursor(_) => (st.cursor_shape == [lazyterm::settings::CursorShape::Block, lazyterm::settings::CursorShape::Underline, lazyterm::settings::CursorShape::Bar][*i], ["Block", "Underline", "Bar"][*i]),
                _ => (st.bell == [lazyterm::settings::Bell::None, lazyterm::settings::Bell::Visual, lazyterm::settings::Bell::Sound, lazyterm::settings::Bell::Notify][*i], ["None", "Visual", "Sound", "Notify"][*i]),
            };
            canvas.fill_rect(rect, color(if on { palette.cursor().lerp(bg, 0.5) } else { button }));
            canvas.draw_text(name, Rect::new(rect.left + 5 * s, rect.top + 4 * s, rect.right, rect.bottom), &TextStyle::new(color(fg), Dip(12.0)));
            let focused = prefs.focus == if matches!(part, Part::Cursor(_)) { Ctl::CursorShape } else { Ctl::Bell };
            if focused && on {
                ring(canvas, rect);
            }
        }
        Part::Toggle(ctl) => {
            let (on, name) = match ctl {
                Ctl::Blink => (st.cursor_blink, "Blink"),
                Ctl::Bold => (st.bold_is_bright, "Bold is bright"),
                Ctl::CopyOnSelect => (st.copy_on_select, "Copy on select"),
                _ => (st.confirm_multiline, "Confirm paste"),
            };
            let boxr = Rect::new(rect.left, rect.top + 3 * s, rect.left + 16 * s, rect.top + 19 * s);
            canvas.fill_rect(boxr, color(button));
            if on {
                canvas.fill_rect(Rect::new(boxr.left + 4 * s, boxr.top + 4 * s, boxr.right - 4 * s, boxr.bottom - 4 * s), focus);
            }
            if prefs.focus == *ctl {
                ring(canvas, boxr);
            }
            canvas.draw_text(name, Rect::new(rect.left + 22 * s, rect.top + 3 * s, rect.right + 40 * s, rect.bottom), text);
        }
        Part::Field(ctl) => {
            let field: &Field = match ctl {
                Ctl::Scrollback => &prefs.scrollback,
                Ctl::WordChars => &prefs.word_chars,
                _ => &prefs.window_size,
            };
            canvas.fill_rect(rect, color(bg));
            if field.text.is_empty() && prefs.focus != *ctl {
                let dim = TextStyle::new(color(fg.lerp(bg, 0.55)), Dip(12.0));
                canvas.draw_text("(default)", Rect::new(rect.left + 5 * s, rect.top + 4 * s, rect.right, rect.bottom), &dim);
            }
            canvas.draw_text(&field.text, Rect::new(rect.left + 5 * s, rect.top + 3 * s, rect.right, rect.bottom), text);
            if prefs.focus == *ctl {
                ring(canvas, rect);
                if caret_on {
                    let x = rect.left + 5 * s + field.caret as i32 * 8 * s;
                    canvas.fill_rect(Rect::new(x, rect.top + 3 * s, x + s, rect.bottom - 3 * s), color(fg));
                }
            }
        }
    }
}
