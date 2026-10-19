//! Drawing the picture on show: the engine's frame and the patches over it, from the top-left corner
//! of the view, moved up by how far the window shows it ahead of the page (`speculate.rs`). What that
//! uncovers is drawn from the bands the page's process painted outside the view (`bands.rs`), and
//! where no band reaches, a checkerboard.

use std::rc::Rc;

use xui_core::backend::Canvas;
use xui_core::geometry::Rect;
use xui_core::{Color, Image};

use crate::bands::{subtract, Bands};
use crate::damage::IRect;
use crate::parts::Parts;

/// Side of a checkerboard square, in pixels.
pub(crate) const SQUARE: i32 = 64;
/// The width of the page's scroll bar (`ScrollbarThemePlayStation::scrollbarThickness`).
pub(crate) const SCROLLBAR: i32 = 7;
pub(crate) const CHECKER_LIGHT: Color = Color::rgb(243, 243, 243);
pub(crate) const CHECKER_DARK: Color = Color::rgb(229, 229, 229);

/// Draws `picture` into the canvas's bounds, moved up by `ahead` pixels. `offset` is the page's scroll
/// offset the picture was painted at, so the checkerboard stays on the page's rows as it scrolls.
/// Returns how many rows of the view were checkerboard.
pub(crate) fn paint(
    canvas: &mut dyn Canvas,
    picture: &Parts,
    bands: &Bands<Rc<Image>>,
    ahead: i32,
    offset: i32,
) -> u32 {
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
    let dy = -ahead;
    if ahead == 0 {
        draw_parts(canvas, picture, bounds, 0);
    } else {
        // The page's scroll bar is in its picture: it must show where the page is, not where the
        // wheel has asked it to go, or the thumb jumps back every time the page answers.
        let body = Rect::new(
            bounds.left,
            bounds.top,
            (bounds.right - SCROLLBAR).max(bounds.left),
            bounds.bottom,
        );
        let bar = Rect::new(body.right, bounds.top, bounds.right, bounds.bottom);
        canvas.push_clip(body);
        draw_parts(canvas, picture, bounds, dy);
        canvas.pop_clip();
        canvas.push_clip(bar);
        draw_parts(canvas, picture, bounds, 0);
        canvas.pop_clip();
    }
    let mut checker_rows = 0;
    if ahead != 0 && !picture.is_empty() {
        let bar_left = (bounds.right - SCROLLBAR).max(bounds.left);
        let top = bounds.top + dy;
        let bottom = bounds.top + height as i32 + dy;
        // The page row at the view's top is where the page will be once it has done what is ahead.
        let page_top = offset.saturating_add(ahead);
        if top > bounds.top {
            let area = Rect::new(bounds.left, bounds.top, bar_left, top.min(bounds.bottom));
            checker_rows += uncovered(canvas, area, bands, bounds.top, page_top, width);
        }
        if bottom < bounds.bottom {
            let area = Rect::new(bounds.left, bottom.max(bounds.top), bar_left, bounds.bottom);
            checker_rows += uncovered(canvas, area, bands, bounds.top, page_top, width);
        }
    }
    canvas.pop_clip();
    checker_rows
}

/// The parts of the picture, each moved down by `dy` (view coordinates, inside whatever clip is set).
fn draw_parts(canvas: &mut dyn Canvas, picture: &Parts, bounds: Rect, dy: i32) {
    let (width, height) = picture.size();
    let frame = IRect::new(0, 0, width as i32, height as i32);
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
}
/// Draws what the picture does not cover in `area` (whole rows of the view): the bands that reach it,
/// oldest first, then a checkerboard in the rows none of them does. `page_top` is the page row at the
/// view's top (`view_top`). Returns the checkerboard's rows.
fn uncovered(
    canvas: &mut dyn Canvas,
    area: Rect,
    bands: &Bands<Rc<Image>>,
    view_top: i32,
    page_top: i32,
    width: u32,
) -> u32 {
    let mut gaps = vec![(area.top, area.bottom)];
    if bands.geometry().0 == width {
        for kept in bands.iter() {
            let top = view_top.saturating_add(kept.doc_y.saturating_sub(page_top));
            let bottom = top.saturating_add(kept.height);
            let (from, to) = (top.max(area.top), bottom.min(area.bottom));
            if from >= to {
                continue;
            }
            canvas.push_clip(Rect::new(area.left, from, area.right, to));
            canvas.draw_image(
                &kept.image,
                Rect::new(area.left, top, area.left + width as i32, bottom),
            );
            canvas.pop_clip();
            subtract(&mut gaps, from, to);
        }
    }
    let mut rows = 0;
    for (from, to) in gaps {
        checkerboard(
            canvas,
            Rect::new(area.left, from, area.right, to),
            view_top,
            page_top,
        );
        rows += (to - from) as u32;
    }
    rows
}

/// Fills `area` with squares fixed to the page: the square at view row `y` is the page's row
/// `page_top + (y - view_top)`, so the pattern moves with the content as it is scrolled.
fn checkerboard(canvas: &mut dyn Canvas, area: Rect, view_top: i32, page_top: i32) {
    canvas.fill_rect(area, CHECKER_LIGHT);
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
                canvas.fill_rect(Rect::new(x, top, right, bottom), CHECKER_DARK);
            }
            x = right;
            column += 1;
        }
        y += SQUARE;
        row += 1;
    }
}
