//! Where things are on the Terminal's node: which cell a pixel is, and the
//! rectangles of the paste confirmation bar. Pure, so it is host-tested.

use lazyterm::{Point, Term};
use xui_core::Rect;

use super::paint::{CELL_W, PAD};
use super::view::{first_row, Layout};

/// Whether a drag left the grid, and on which side (it autoscrolls).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edge {
    Inside,
    Above,
    Below,
}

/// The pixel metrics of `layout`: the UI scale and the cell width.
fn metrics(layout: &Layout) -> (i32, f32) {
    let scale = (layout.pad / PAD).max(1);
    (scale, CELL_W * scale as f32)
}

/// The column and view slot (negative above the grid, `>= visible` below it)
/// of node pixel (`x`, `y`).
pub fn cell_of(layout: &Layout, x: i32, y: i32) -> (usize, i64) {
    let (_, cell_w) = metrics(layout);
    let col = ((x - layout.pad) as f32 / cell_w).floor().max(0.0) as usize;
    let slot = i64::from((y - layout.pad).div_euclid(layout.line_h.max(1)));
    (col, slot)
}

/// The scrollback point under node pixel (`x`, `y`) with the view scrolled
/// back `offset` lines, clamped into the view, and whether the pixel was
/// outside it.
pub fn point_at(layout: &Layout, term: &Term, offset: usize, x: i32, y: i32) -> (Point, Edge) {
    let (col, slot) = cell_of(layout, x, y);
    let visible = layout.visible.clamp(1, term.rows());
    let edge = if slot < 0 {
        Edge::Above
    } else if slot >= visible as i64 {
        Edge::Below
    } else {
        Edge::Inside
    };
    let slot = slot.clamp(0, visible as i64 - 1) as u64;
    let first = first_row(term.cursor().0, visible, term.rows()) as u64;
    let line = (term.screen_top() + first + slot).saturating_sub(offset as u64);
    let col = col.min(term.cols() - 1);
    (Point::new(line.max(term.first_line()), col), edge)
}

/// The screen cell (column, row) under node pixel (`x`, `y`), for mouse
/// reports: the program sees the screen, not the scrollback.
pub fn screen_cell(layout: &Layout, term: &Term, x: i32, y: i32) -> (usize, usize) {
    let (col, slot) = cell_of(layout, x, y);
    let visible = layout.visible.clamp(1, term.rows());
    let first = first_row(term.cursor().0, visible, term.rows());
    let slot = slot.clamp(0, visible as i64 - 1) as usize;
    (col.min(term.cols() - 1), first + slot)
}

/// The paste confirmation bar along the bottom of the node and its buttons,
/// in node pixels.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Confirm {
    pub bar: Rect,
    pub paste: Rect,
    pub cancel: Rect,
}

pub fn confirm_rects(layout: &Layout) -> Confirm {
    let unit = layout.pad;
    let bar = Rect::new(0, layout.height - 3 * unit, layout.width, layout.height);
    let (width, height) = (8 * unit, 2 * unit);
    let top = bar.top + unit / 2;
    let cancel = Rect::new(layout.width - unit - width, top, layout.width - unit, top + height);
    let paste = Rect::new(cancel.left - unit - width, top, cancel.left - unit, top + height);
    Confirm { bar, paste, cancel }
}

/// Whether node pixel (`x`, `y`) is inside `rect`.
pub fn contains(rect: Rect, x: i32, y: i32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAYOUT: Layout = Layout {
        width: 640,
        height: 400,
        pad: 10,
        line_h: 20,
        visible: 19,
    };

    fn full_term() -> Term {
        let mut term = Term::new(80, 19);
        for line in 1..=60 {
            term.feed(format!("line {line}\r\n").as_bytes());
        }
        term
    }

    #[test]
    fn pixels_map_to_cells() {
        assert_eq!(cell_of(&LAYOUT, 10, 10), (0, 0));
        assert_eq!(cell_of(&LAYOUT, 10 + 9 * 5 + 4, 10 + 20 * 3 + 19), (5, 3));
        assert_eq!(cell_of(&LAYOUT, 2, 2), (0, -1), "above and left of the grid");
        assert_eq!(cell_of(&LAYOUT, 600, 395), (65, 19));
    }

    #[test]
    fn the_scale_scales_the_cells() {
        let two = Layout { pad: 20, line_h: 40, ..LAYOUT };
        assert_eq!(cell_of(&two, 20 + 18 * 5 + 2, 20 + 40 * 2), (5, 2));
    }

    #[test]
    fn a_point_is_an_absolute_line_in_the_scrolled_view() {
        let term = full_term();
        let (bottom, edge) = point_at(&LAYOUT, &term, 0, 10, 10 + 20 * 18 + 5);
        assert_eq!(edge, Edge::Inside);
        assert_eq!(bottom.line, term.screen_top() + 18, "the last screen row");
        let (back, _) = point_at(&LAYOUT, &term, 7, 10, 10 + 20 * 18 + 5);
        assert_eq!(back.line, bottom.line - 7, "scrolled back 7 lines");
        let (top, _) = point_at(&LAYOUT, &term, 0, 10, 10);
        assert_eq!(top.line, term.screen_top());
    }

    #[test]
    fn a_drag_past_the_edges_is_clamped_and_reported() {
        let term = full_term();
        let (above, edge) = point_at(&LAYOUT, &term, 0, 30, -50);
        assert_eq!((edge, above.line), (Edge::Above, term.screen_top()));
        let (below, edge) = point_at(&LAYOUT, &term, 0, 30, 5000);
        assert_eq!((edge, below.line), (Edge::Below, term.screen_top() + 18));
        let (right, _) = point_at(&LAYOUT, &term, 0, 5000, 10);
        assert_eq!(right.col, 79);
        let (way_back, _) = point_at(&LAYOUT, &term, 100_000, 10, 10);
        assert_eq!(way_back.line, term.first_line());
    }

    #[test]
    fn mouse_reports_name_screen_cells() {
        let term = full_term();
        assert_eq!(screen_cell(&LAYOUT, &term, 10 + 9 * 7, 10 + 20 * 2), (7, 2));
        assert_eq!(screen_cell(&LAYOUT, &term, -5, -5), (0, 0));
    }

    #[test]
    fn the_confirmation_bar_has_two_buttons_inside_it() {
        let c = confirm_rects(&LAYOUT);
        assert_eq!(c.bar, Rect::new(0, 370, 640, 400));
        for button in [c.paste, c.cancel] {
            assert!(button.left >= c.bar.left && button.right <= c.bar.right);
            assert!(button.top >= c.bar.top && button.bottom <= c.bar.bottom);
        }
        assert!(c.paste.right < c.cancel.left, "side by side, Cancel at the right");
        assert!(contains(c.paste, c.paste.left, c.paste.top));
        assert!(!contains(c.paste, c.paste.right, c.paste.top));
    }
}
