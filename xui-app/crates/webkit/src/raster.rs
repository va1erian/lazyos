//! The picture on show, written straight into a window's pixels: what `picture.rs` draws through a canvas,
//! as row copies (the LazyOS backend's direct painters, `xui-app/src/backend/direct.rs`).
//!
//! A scroll changes every pixel of the page view, 3 to 4 megapixels a frame at 2560x1440. Through the canvas
//! that is several passes (xui prepares each new image, a partial repaint goes through a scratch copy, the
//! window background is drawn underneath); here it is one copy of each row from the images the picture
//! is made of. The images are opaque (WebKit's frames and bands), so their RGBA is also premultiplied RGBA.
//! The rules are `picture.rs`'s: the picture moved up by `ahead` except the scroll bar's columns, then the
//! bands where it does not reach, then a checkerboard; `tests_raster.rs` compares the two pixel for pixel.

use std::rc::Rc;

use xui_core::Image;

use crate::bands::{subtract, Bands};
use crate::damage::IRect;
use crate::parts::Parts;
use crate::picture::{CHECKER_DARK, CHECKER_LIGHT, SCROLLBAR, SQUARE};

/// Pixels to write: `stride` bytes a row, RGBA.
pub struct Pixels<'a> {
    pub data: &'a mut [u8],
    pub stride: usize,
}

impl Pixels<'_> {
    fn fill(&mut self, rect: IRect, rgba: [u8; 4]) {
        for y in rect.y0..rect.y1 {
            let start = y as usize * self.stride + rect.x0 as usize * 4;
            let row = &mut self.data[start..start + rect.width() as usize * 4];
            for px in row.chunks_exact_mut(4) {
                px.copy_from_slice(&rgba);
            }
        }
    }

    /// Copies the part of `image` (its top-left corner at `x, y`) that falls in `clip`.
    fn blit(&mut self, image: &Image, x: i32, y: i32, clip: IRect) {
        let whole = IRect::new(x, y, image.width() as i32, image.height() as i32);
        let r = whole.intersect(&clip);
        if r.is_empty() {
            return;
        }
        let source = image.pixels();
        let source_stride = image.width() as usize * 4;
        let run = r.width() as usize * 4;
        for row in r.y0..r.y1 {
            let from = (row - y) as usize * source_stride + (r.x0 - x) as usize * 4;
            let to = row as usize * self.stride + r.x0 as usize * 4;
            self.data[to..to + run].copy_from_slice(&source[from..from + run]);
        }
    }
}

/// Writes the part `damage` of the page view at `view` (both in the pixels' coordinates; `damage` inside
/// `view` and inside the pixels) as `picture::paint` would draw it. Returns the checkerboard's rows.
pub fn raster(
    out: &mut Pixels<'_>,
    view: IRect,
    damage: IRect,
    picture: &Parts,
    bands: &Bands<Rc<Image>>,
    ahead: i32,
    offset: i32,
) -> u32 {
    let damage = damage.intersect(&view);
    if damage.is_empty() {
        return 0;
    }
    let (width, height) = picture.size();
    let covered =
        !picture.is_empty() && width as i32 >= view.width() && height as i32 >= view.height();
    if !covered {
        out.fill(damage, [255, 255, 255, 255]);
    }
    let dy = -ahead;
    if ahead == 0 {
        draw_parts(out, picture, view, damage, 0);
        return 0;
    }
    let bar_left = (view.x1 - SCROLLBAR).max(view.x0);
    let body = IRect {
        x0: view.x0,
        y0: view.y0,
        x1: bar_left,
        y1: view.y1,
    };
    let bar = IRect {
        x0: bar_left,
        y0: view.y0,
        x1: view.x1,
        y1: view.y1,
    };
    draw_parts(out, picture, view, damage.intersect(&body), dy);
    draw_parts(out, picture, view, damage.intersect(&bar), 0);
    if picture.is_empty() {
        return 0;
    }
    let top = view.y0 + dy;
    let bottom = view.y0 + height as i32 + dy;
    let page_top = offset.saturating_add(ahead);
    let mut rows = 0;
    for (from, to) in [(view.y0, top.min(view.y1)), (bottom.max(view.y0), view.y1)] {
        let area = IRect {
            x0: view.x0,
            y0: from,
            x1: bar_left,
            y1: to,
        }
        .intersect(&damage);
        if !area.is_empty() {
            rows += uncovered(out, area, bands, view.y0, page_top, width, view.x0);
        }
    }
    rows
}

/// The picture's parts moved down by `dy`, inside `clip` (all in the pixels' coordinates).
fn draw_parts(out: &mut Pixels<'_>, picture: &Parts, view: IRect, clip: IRect, dy: i32) {
    if clip.is_empty() {
        return;
    }
    let (width, height) = picture.size();
    let frame = IRect::new(0, 0, width as i32, height as i32);
    for part in picture.parts() {
        let shown = part.visible(&frame);
        if shown.is_empty() {
            continue;
        }
        let shown = shown.translate(view.x0, view.y0 + dy).intersect(&clip);
        out.blit(&part.image, view.x0 + part.x, view.y0 + part.y + dy, shown);
    }
}

/// The bands that reach `area` (whole rows of the view, oldest first), then a checkerboard where none does.
fn uncovered(
    out: &mut Pixels<'_>,
    area: IRect,
    bands: &Bands<Rc<Image>>,
    view_top: i32,
    page_top: i32,
    width: u32,
    left: i32,
) -> u32 {
    let mut gaps = vec![(area.y0, area.y1)];
    if bands.geometry().0 == width {
        for kept in bands.iter() {
            let top = view_top.saturating_add(kept.doc_y.saturating_sub(page_top));
            let bottom = top.saturating_add(kept.height);
            let (from, to) = (top.max(area.y0), bottom.min(area.y1));
            if from >= to {
                continue;
            }
            out.blit(
                &kept.image,
                left,
                top,
                IRect {
                    x0: area.x0,
                    y0: from,
                    x1: area.x1,
                    y1: to,
                },
            );
            subtract(&mut gaps, from, to);
        }
    }
    let mut rows = 0;
    for (from, to) in gaps {
        checkerboard(
            out,
            IRect {
                x0: area.x0,
                y0: from,
                x1: area.x1,
                y1: to,
            },
            view_top,
            page_top,
            left,
        );
        rows += (to - from) as u32;
    }
    rows
}

/// `picture::checkerboard`'s squares: fixed to the page's rows, columns from the view's left edge.
fn checkerboard(out: &mut Pixels<'_>, area: IRect, view_top: i32, page_top: i32, left: i32) {
    let light = rgba(CHECKER_LIGHT);
    let dark = rgba(CHECKER_DARK);
    for y in area.y0..area.y1 {
        let page_row = page_top + (y - view_top);
        let square_row = page_row.div_euclid(SQUARE);
        let mut x = area.x0;
        while x < area.x1 {
            let column = (x - left).div_euclid(SQUARE);
            let end = (left + (column + 1) * SQUARE).min(area.x1);
            let color = if (square_row + column) & 1 != 0 {
                dark
            } else {
                light
            };
            out.fill(
                IRect {
                    x0: x,
                    y0: y,
                    x1: end,
                    y1: y + 1,
                },
                color,
            );
            x = end;
        }
    }
}

fn rgba(color: xui_core::Color) -> [u8; 4] {
    [color.r, color.g, color.b, 255]
}
