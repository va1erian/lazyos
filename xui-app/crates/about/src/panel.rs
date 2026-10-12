//! The text panel over the 3D view: where it sits for a window size, and
//! how it is painted (the frame first, then a translucent panel with the
//! title, the page, its dots and the key hints, in the canvas's own text).

use xui_core::backend::{Canvas, Rgba, TextStyle};
use xui_core::geometry::Rect;
use xui_core::{Color, Dip};

use crate::pages::{HINT, PAGES};
use crate::view::State;

/// Window pixels, split into the 3D area and the text panel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub area: (i32, i32, i32, i32),
    pub panel: (i32, i32, i32, i32),
}

/// A wide window puts the panel on the right; a narrow one, along the
/// bottom.
pub fn layout(w: i32, h: i32, scale: i32) -> Layout {
    if w >= 620 * scale {
        let pw = ((w as f32 * 0.4) as i32).clamp(300 * scale, 440 * scale);
        Layout {
            area: (0, 0, w - pw, h),
            panel: (w - pw, 0, w, h),
        }
    } else {
        let ph = h / 2;
        Layout {
            area: (0, 0, w, h - ph),
            panel: (0, h - ph, w, h),
        }
    }
}

const INK: Color = Color::hex(0xF2ECFF);
const DIM: Color = Color::hex(0xA99BD0);
const CYAN: Color = Color::hex(0x35E6FF);
const PINK: Color = Color::hex(0xFF4FC3);
const GOLD: Color = Color::hex(0xFFD95A);

/// Paints the whole widget: the frame, then the panel over it.
pub fn paint(canvas: &mut dyn Canvas, st: &State) {
    let b = canvas.bounds();
    let s = (canvas.dpi() as i32 / 96).max(1);
    match &st.image {
        Some(image) => canvas.draw_image(image, b),
        None => canvas.fill_rect(b, Color::hex(0x120A2A)),
    }
    let lay = layout(b.width(), b.height(), s);
    let panel = Rect::new(
        b.left + lay.panel.0,
        b.top + lay.panel.1,
        b.left + lay.panel.2,
        b.top + lay.panel.3,
    );
    canvas.fill_rect_rgba(panel, Rgba::with_alpha(10, 4, 30, 176));
    let edge = if lay.panel.0 > 0 {
        Rect::new(panel.left, panel.top, panel.left + s, panel.bottom)
    } else {
        Rect::new(panel.left, panel.top, panel.right, panel.top + s)
    };
    canvas.fill_rect(edge, CYAN);

    let pad = 22 * s;
    let x0 = panel.left + pad + if lay.panel.0 > 0 { s * 2 } else { 0 };
    let x1 = panel.right - pad;
    let mut y = panel.top + pad;
    let narrow = lay.panel.0 == 0;
    let line =
        |canvas: &mut dyn Canvas, y: i32, text: &str, size: f32, color: Color, bold: bool| {
            let mut style = TextStyle::new(color, Dip(size));
            if bold {
                style = style.bold();
            }
            let height = (size * 1.35) as i32 * s;
            canvas.draw_text(text, Rect::new(x0, y, x1, y + height), &style);
            y + height
        };
    y = line(
        canvas,
        y,
        "LazyOS",
        if narrow { 26.0 } else { 38.0 },
        CYAN,
        true,
    );
    y = line(
        canvas,
        y,
        "An operating system, from scratch, in Rust",
        13.0,
        PINK,
        false,
    );
    y = line(
        canvas,
        y,
        &format!(
            "Version {}  \u{b7}  x86_64  \u{b7}  Messenger IPC",
            st.info.version
        ),
        11.0,
        DIM,
        false,
    );
    y += 14 * s;

    let page = &PAGES[st.page.min(PAGES.len() - 1)];
    y = line(canvas, y, page.title, 18.0, GOLD, true);
    y += 4 * s;
    let foot = panel.bottom - pad - 28 * s;
    let dots_y = foot - 10 * s;
    let body = Rect::new(x0, y, x1, (dots_y - 10 * s).max(y));
    canvas.draw_text(page.body, body, &TextStyle::new(INK, Dip(13.0)).wrapped());

    // Page dots: the current one a wide bar.
    let mut dx = x0;
    for i in 0..PAGES.len() {
        let wide = if i == st.page { 18 } else { 6 } * s;
        let color = if i == st.page {
            PINK
        } else {
            Color::hex(0x5B4B8A)
        };
        canvas.fill_rect(Rect::new(dx, dots_y, dx + wide, dots_y + 5 * s), color);
        dx += wide + 5 * s;
    }
    for (i, hint) in HINT.iter().enumerate() {
        let top = foot + i as i32 * 14 * s;
        canvas.draw_text(
            hint,
            Rect::new(x0, top, x1, top + 14 * s),
            &TextStyle::new(DIM, Dip(10.0)),
        );
    }

    // What is on show, bottom-left of the 3D area.
    let area_bottom = b.top + lay.area.3;
    canvas.draw_text(
        &st.status(),
        Rect::new(
            b.left + 12 * s,
            area_bottom - 22 * s,
            b.left + lay.area.2 - 8 * s,
            area_bottom - 6 * s,
        ),
        &TextStyle::new(Color::hex(0xD7C8FF), Dip(11.0)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_puts_the_panel_beside_or_below() {
        let wide = layout(1000, 600, 1);
        assert_eq!(wide.panel.2, 1000);
        assert_eq!(wide.area.2, wide.panel.0);
        let narrow = layout(500, 600, 1);
        assert_eq!(narrow.area.2, 500);
        assert_eq!(narrow.panel.1, narrow.area.3);
    }
}
