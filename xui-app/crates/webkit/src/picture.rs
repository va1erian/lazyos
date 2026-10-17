//! Drawing the picture on show: the engine's frame and the patches over it, from the top-left corner
//! of the view, moved up by how far the window shows it ahead of the page (`speculate.rs`) with a
//! checkerboard where that uncovers what the page has not painted yet.

use xui_core::backend::Canvas;
use xui_core::geometry::Rect;
use xui_core::Color;

use crate::damage::IRect;
use crate::parts::Parts;

/// Side of a checkerboard square, in pixels.
const SQUARE: i32 = 64;
const LIGHT: Color = Color::rgb(243, 243, 243);
const DARK: Color = Color::rgb(229, 229, 229);

/// Draws `picture` into the canvas's bounds, moved up by `ahead` pixels. `offset` is the page's scroll
/// offset the picture was painted at, so the checkerboard stays on the page's rows as it scrolls.
pub(crate) fn paint(canvas: &mut dyn Canvas, picture: &Parts, ahead: i32, offset: i32) {
    let bounds = canvas.bounds();
    canvas.push_clip(bounds);
    // White only where the picture does not reach (a window being resized): a frame is opaque and
    // covers the view, and filling under it was a whole extra pass over every repaint.
    let (width, height) = picture.size();
    let covered =
        !picture.is_empty() && width as i32 >= bounds.width() && height as i32 >= bounds.height();
    if !covered {
        canvas.fill_rect(bounds, Color::rgb(255, 255, 255));
    }
    let frame = IRect::new(0, 0, width as i32, height as i32);
    let dy = -ahead;
    for part in picture.parts() {
        let shown = part.visible(&frame);
        if shown.is_empty() {
            continue;
        }
        let whole = IRect::new(
            part.x,
            part.y + dy,
            part.image.width() as i32,
            part.image.height() as i32,
        );
        let at = Rect::new(
            bounds.left + whole.x0,
            bounds.top + whole.y0,
            bounds.left + whole.x1,
            bounds.top + whole.y1,
        );
        let shown = shown.translate(0, dy);
        if shown == whole {
            canvas.draw_image(&part.image, at);
        } else {
            // Only part of this image is on show: a picture that scrolled, or a patch that was
            // partly covered. Drawn whole, behind a clip.
            canvas.push_clip(Rect::new(
                bounds.left + shown.x0,
                bounds.top + shown.y0,
                bounds.left + shown.x1,
                bounds.top + shown.y1,
            ));
            canvas.draw_image(&part.image, at);
            canvas.pop_clip();
        }
    }
    if ahead != 0 && !picture.is_empty() {
        let top = bounds.top + dy;
        let bottom = bounds.top + height as i32 + dy;
        // The page row at the view's top is where the page will be once it has done what is ahead.
        let page_top = offset + ahead;
        if top > bounds.top {
            checkerboard(
                canvas,
                Rect::new(
                    bounds.left,
                    bounds.top,
                    bounds.right,
                    top.min(bounds.bottom),
                ),
                bounds.top,
                page_top,
            );
        }
        if bottom < bounds.bottom {
            checkerboard(
                canvas,
                Rect::new(
                    bounds.left,
                    bottom.max(bounds.top),
                    bounds.right,
                    bounds.bottom,
                ),
                bounds.top,
                page_top,
            );
        }
    }
    canvas.pop_clip();
}

/// Fills `area` with squares fixed to the page: the square at view row `y` is the page's row
/// `page_top + (y - view_top)`, so the pattern moves with the content as it is scrolled.
fn checkerboard(canvas: &mut dyn Canvas, area: Rect, view_top: i32, page_top: i32) {
    canvas.fill_rect(area, LIGHT);
    let first = page_top + (area.top - view_top);
    let mut row = first.div_euclid(SQUARE);
    let mut y = area.top - first.rem_euclid(SQUARE);
    while y < area.bottom {
        let top = y.max(area.top);
        let bottom = (y + SQUARE).min(area.bottom);
        let mut x = area.left;
        let mut column = 0;
        while x < area.right {
            let right = (x + SQUARE).min(area.right);
            if (row + column) & 1 != 0 {
                canvas.fill_rect(Rect::new(x, top, right, bottom), DARK);
            }
            x = right;
            column += 1;
        }
        y += SQUARE;
        row += 1;
    }
}
