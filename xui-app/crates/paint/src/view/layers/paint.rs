#![forbid(unsafe_code)]

//! Drawing the Layers panel.

use xui_core::Theme;
use xui_core::backend::{Canvas, TextStyle};
use xui_core::geometry::Rect;
use xui_core::icon::{Lucide, draw_icon};
use xui_core::theme::look;

use super::{Hit, State};

/// The name's design font size.
const FONT: xui_core::Dip = xui_core::Dip(12.0);

pub(super) fn paint(canvas: &mut dyn Canvas, state: &State, theme: &Theme) {
    let bounds = state.bounds.get();
    canvas.fill_rect(bounds, theme.input_background);
    canvas.push_clip(bounds);
    let rows = state.rows.borrow();
    let active = state.active.get();
    let hover = state.hover.get();
    for (layer, row) in rows.iter().enumerate().rev() {
        let rect = state.row_rect(layer);
        if rect.bottom < bounds.top || rect.top > bounds.bottom {
            continue;
        }
        let on = layer == active;
        if on {
            look::selected_row(canvas, rect, theme);
        } else if hover == Some(Hit::Row(layer)) || hover == Some(Hit::Eye(layer)) {
            look::row(canvas, rect, theme.hover, theme);
        }
        let ink = if on {
            look::selected_row_text(theme)
        } else if row.visible {
            theme.text
        } else {
            theme.text_disabled
        };

        let eye = state.eye_rect(layer);
        let icon = if row.visible {
            Lucide::Eye
        } else {
            Lucide::EyeOff
        };
        draw_icon(canvas, icon, eye, ink, state.dpi.get());

        let thumb = state.thumb_box(layer);
        if let Some(image) = &row.thumb {
            let (w, h) = (image.width() as i32, image.height() as i32);
            let left = thumb.left + (thumb.width() - w) / 2;
            let top = thumb.top + (thumb.height() - h) / 2;
            let target = Rect::new(left, top, left + w, top + h);
            canvas.draw_image(image, target);
            canvas.stroke_rect(target, theme.border, 1.0);
        }

        let full = state.name_rect(layer);
        let faded = row.opacity < 255 && row.visible;
        // A faded layer's percent sits under the name.
        let name = if faded {
            Rect::new(
                full.left,
                full.top,
                full.right,
                full.bottom - full.height() / 3,
            )
        } else {
            full
        };
        let style = TextStyle::new(ink, FONT).middle();
        canvas.push_clip(name);
        canvas.draw_text(&row.name, name, &style);
        canvas.pop_clip();
        if faded {
            let percent = (u32::from(row.opacity) * 100 + 127) / 255;
            let small = TextStyle::new(theme.text_secondary, xui_core::Dip(10.0));
            let line = Rect::new(full.left, name.bottom - 2, full.right, full.bottom);
            canvas.draw_text(&format!("{percent}%"), line, &small);
        }
    }
    canvas.pop_clip();
    canvas.stroke_rect(bounds, theme.border, 1.0);
}
