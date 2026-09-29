#![forbid(unsafe_code)]

//! Drawing an icon on a [`Canvas`].

use xui_core::backend::{Canvas, Cap, Join, PathPlacement, Stroke};
use xui_core::geometry::Rect;

use crate::data::Village;
use crate::tone::Palette;

/// The design grid every icon is authored on, in path units.
pub const GRID: f32 = 32.0;

/// The thinnest stroke drawn, in device pixels, so the outline of a small icon
/// stays visible instead of fading to a faint line.
const MIN_STROKE_PX: f32 = 0.75;

/// Draws `icon` centred in `rect`, scaled so the grid spans the rectangle's
/// smaller side, in the colours of `palette`.
///
/// It goes through [`Canvas::fill_path`] and [`Canvas::stroke_path`] only, so
/// every backend draws it: `xui-canvas` rasterises it with `tiny-skia`,
/// `xui-win32` with Direct2D. Nothing is drawn for an empty rectangle. Icons are
/// multi-coloured, so they take a [`Palette`] rather than a single colour.
///
/// ```no_run
/// use xui_icons::{Palette, Village, draw};
/// # fn paint(canvas: &mut dyn xui_core::backend::Canvas, rect: xui_core::Rect) {
/// draw(canvas, Village::Folder, rect, &Palette::GLOBAL_VILLAGE);
/// # }
/// ```
pub fn draw(canvas: &mut dyn Canvas, icon: Village, rect: Rect, palette: &Palette) {
    let size = rect.width().min(rect.height());
    if size <= 0 {
        return;
    }
    let scale = size as f32 / GRID;
    let at = PathPlacement::new(
        scale,
        (rect.left + rect.right - size) as f32 / 2.0,
        (rect.top + rect.bottom - size) as f32 / 2.0,
    );
    for shape in icon.shapes() {
        if let Some(fill) = shape.fill {
            canvas.fill_path(shape.path, at, palette.get(fill));
        }
        if let Some(line) = shape.line {
            let stroke = Stroke::new((line.width * scale).max(MIN_STROKE_PX))
                .cap(Cap::Round)
                .join(Join::Round);
            canvas.stroke_path(shape.path, at, palette.get(line.tone), &stroke);
        }
    }
}
