#![forbid(unsafe_code)]

//! Turning the flattened picture into the pixels on screen.
//!
//! xui's `draw_image` always filters bilinearly (GAPS G5), which would blur
//! zoomed-in pixels. So the canvas builds the screen image itself: only the
//! visible window, scaled nearest-neighbour, over a checkerboard where the
//! picture is not opaque, with a pixel grid when zoomed far in, and draws it 1:1.
//! The cost per frame follows the window, not the picture.

use crate::model::{Bitmap, Damage, Document, div255};
use crate::viewport::{GRID_ZOOM, Viewport};

/// The checkerboard cell edge in view pixels.
const CHECKER: i64 = 8;
/// The two checkerboard greys.
const LIGHT: u8 = 255;
const DARK: u8 = 204;

/// The composite of the visible layers, kept current by dirty rectangles.
#[derive(Default)]
pub struct FlatCache {
    flat: Option<Bitmap>,
}

impl FlatCache {
    /// An empty cache; the first [`FlatCache::update`] builds it.
    pub fn new() -> FlatCache {
        FlatCache::default()
    }

    /// Brings the cache up to date with `doc` given what changed since the last
    /// update, and returns it. A size change or `All` rebuilds it; a rectangle
    /// recomputes only that rectangle.
    pub fn update(&mut self, doc: &Document, damage: Damage) -> &Bitmap {
        let stale = self.flat.as_ref().is_none_or(|f| f.size() != doc.size());
        match (stale, damage) {
            (true, _) | (_, Damage::All) => self.flat = Some(doc.flatten()),
            (false, Damage::Rect(rect)) => {
                if let Some(flat) = &mut self.flat {
                    doc.flatten_into(flat, rect);
                }
            }
            (false, Damage::None) => {}
        }
        self.flat.get_or_insert_with(|| doc.flatten())
    }

    /// The cached picture, if built.
    pub fn get(&self) -> Option<&Bitmap> {
        self.flat.as_ref()
    }
}

/// The window-sized image: opaque RGBA8, row-major.
#[derive(Debug, PartialEq, Eq)]
pub struct ScreenImage {
    /// Width in view pixels.
    pub width: u32,
    /// Height in view pixels.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// Builds the image of `flat` as seen through `viewport` in a `view`-sized
/// window, or `None` when nothing of the picture is visible.
pub fn render(flat: &Bitmap, viewport: &Viewport, view: (i32, i32), grid: bool) -> Option<ScreenImage> {
    let (content_w, content_h) = viewport.content_size(flat.size());
    let (ox, oy) = viewport.origin();
    let width = view.0.min(content_w.saturating_sub(ox));
    let height = view.1.min(content_h.saturating_sub(oy));
    if width <= 0 || height <= 0 {
        return None;
    }
    let last = (flat.width() as i32 - 1, flat.height() as i32 - 1);
    // Which picture column or row each view column or row samples.
    let cols: Vec<i32> = (0..width).map(|x| viewport.to_doc(x, 0).0.clamp(0, last.0)).collect();
    let show_grid = grid && viewport.zoom() >= GRID_ZOOM;
    let edge_x = |x: i32| show_grid && viewport.to_doc(x - 1, 0).0 != viewport.to_doc(x, 0).0;
    let edge_y = |y: i32| show_grid && viewport.to_doc(0, y - 1).1 != viewport.to_doc(0, y).1;
    let col_edges: Vec<bool> = (0..width).map(edge_x).collect();

    let mut rgba = vec![0u8; width as usize * height as usize * 4];
    for (y, out_row) in rgba.chunks_exact_mut(width as usize * 4).enumerate() {
        let y = y as i32;
        let src_y = viewport.to_doc(0, y).1.clamp(0, last.1);
        let src = flat.row(src_y, 0, flat.width() as usize);
        let row_edge = edge_y(y);
        let checker_row = (i64::from(y) + i64::from(oy)).div_euclid(CHECKER);
        for (x, out) in out_row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let at = cols[x] as usize * 4;
            let [r, g, b, a] = [src[at], src[at + 1], src[at + 2], src[at + 3]];
            *out = if a == 255 {
                [r, g, b, 255]
            } else {
                let cell = (x as i64 + i64::from(ox)).div_euclid(CHECKER) + checker_row;
                let grey = u32::from(if cell & 1 == 0 { LIGHT } else { DARK });
                let a = u32::from(a);
                let mix = |c: u8| div255(u32::from(c) * a + grey * (255 - a)) as u8;
                [mix(r), mix(g), mix(b), 255]
            };
            if row_edge || col_edges[x] {
                // A mid-grey line that shows on both light and dark pixels.
                for c in &mut out[..3] {
                    *c = ((u16::from(*c) + 128) / 2) as u8;
                }
            }
        }
    }
    Some(ScreenImage {
        width: width as u32,
        height: height as u32,
        rgba,
    })
}

#[cfg(test)]
mod tests;
