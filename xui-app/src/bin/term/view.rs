//! What the Terminal has put on screen, so a drain repaints only the grid
//! rows that changed (every row when the view scrolled) instead of the whole
//! window, and so the painter draws only the rows inside the damage.
//!
//! The view is a window of `visible` lines. At scroll offset 0 it ends at the
//! screen's bottom (following the cursor on a screen taller than the window);
//! at offset `s` it sits `s` lines higher, into the scrollback. Lines are
//! addressed by absolute number (`Term::line`), so the offset is the only
//! scroll state.

use lazyterm::{Cell, LineRef, Term};
use xui_core::Rect;

/// Where the painter last laid the grid out, in pixels relative to the
/// Terminal's node: the update turns changed rows into a rectangle with it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Layout {
    pub width: i32,
    pub height: i32,
    pub pad: i32,
    pub line_h: i32,
    /// Grid rows that fit the window.
    pub visible: usize,
}

impl Layout {
    /// The node-relative band of screen slots `first..=last`.
    fn rows(&self, first: usize, last: usize) -> Rect {
        let top = self.pad + first as i32 * self.line_h;
        let bottom = self.pad + (last as i32 + 1) * self.line_h;
        Rect::new(0, top, self.width, bottom.min(self.height))
    }

    /// The gutter the scrollbar sits in (the right padding).
    pub fn strip(&self) -> Rect {
        Rect::new(self.width - self.pad, 0, self.width, self.height)
    }
}

/// The first screen row shown of `rows`: the top while the screen is not
/// full, then the rows ending at the cursor so the newest line stays visible.
pub fn first_row(cursor_row: usize, visible: usize, rows: usize) -> usize {
    if cursor_row < visible {
        0
    } else {
        (cursor_row + 1 - visible).min(rows - visible)
    }
}

/// How far back the view can go: the scrollback plus the screen rows above
/// the window (`first` of them), so the oldest line can reach the top.
pub fn max_offset(term: &Term, visible: usize) -> usize {
    if term.on_alternate_screen() {
        return 0;
    }
    let first = first_row(term.cursor().0, visible.clamp(1, term.rows()), term.rows());
    term.scrollback_len() + first
}

/// The scrollback line a view shows in slot `slot`: `first` is the screen row
/// at the top of the view when it is at the bottom, `offset` how many lines
/// back it is scrolled. `None` is a blank slot.
pub fn view_line(term: &Term, first: usize, offset: usize, slot: usize) -> Option<LineRef<'_>> {
    term.line(line_number(term, first, offset, slot)?)
}

/// The absolute number of the line in view slot `slot`.
pub fn line_number(term: &Term, first: usize, offset: usize, slot: usize) -> Option<u64> {
    (term.screen_top() + first as u64 + slot as u64).checked_sub(offset as u64)
}

/// The scrollbar thumb as (top, bottom) inside a track of `track_h` pixels
/// from `track_top`: `history` lines are above the `visible` shown at the
/// bottom, and `offset` of them are scrolled into view. `None` at offset 0.
pub fn thumb(
    track_top: i32,
    track_h: i32,
    history: usize,
    offset: usize,
    visible: usize,
    min_h: i32,
) -> Option<(i32, i32)> {
    if offset == 0 || history == 0 || track_h <= 0 {
        return None;
    }
    let total = (history + visible) as f32;
    let height = ((track_h as f32 * visible as f32 / total) as i32)
        .max(min_h)
        .min(track_h);
    let travel = (track_h - height) as f32;
    let at = (history - offset.min(history)) as f32 / history as f32;
    let top = track_top + (travel * at) as i32;
    Some((top, top + height))
}

/// The cells up to the last non-blank one: a scrollback line is stored
/// trimmed and a screen row is not, and the same text must compare equal.
fn used(cells: &[Cell]) -> Vec<Cell> {
    let end = cells
        .iter()
        .rposition(|cell| *cell != Cell::default())
        .map_or(0, |at| at + 1);
    cells[..end].to_vec()
}

/// The rectangle covering both.
fn union(a: Option<Rect>, b: Option<Rect>) -> Option<Rect> {
    match (a, b) {
        (Some(a), Some(b)) => Some(Rect::new(
            a.left.min(b.left),
            a.top.min(b.top),
            a.right.max(b.right),
            a.bottom.max(b.bottom),
        )),
        (a, b) => a.or(b),
    }
}

/// The screen as last invalidated: the layout, the first row, the scroll
/// offset, the visible lines' cells and the cursor's slot, column and
/// visibility.
#[derive(Default)]
pub struct Shown {
    layout: Option<Layout>,
    first: usize,
    /// The absolute line at the top of the view: it moves when the view
    /// scrolls, not when the offset merely follows new output.
    anchor: u64,
    /// The scrollbar's inputs (history lines, offset) while it is up.
    bar: Option<(usize, usize)>,
    rows: Vec<Vec<Cell>>,
    /// The selected columns of each visible slot.
    spans: Vec<Option<(usize, usize)>>,
    /// The cursor's slot, column and visibility; `None` while it is not in
    /// the view (scrolled out of it).
    cursor: Option<(usize, usize, bool)>,
}

impl Shown {
    /// The node-relative rectangle to repaint so the screen shows `term` laid
    /// out by `layout` at scroll `offset` (`None` when nothing visible
    /// changed), remembering it as shown. Without a layout yet (nothing
    /// painted) the whole node.
    pub fn update(&mut self, term: &Term, layout: Option<Layout>, offset: usize) -> Option<Rect> {
        let Some(layout) = layout else {
            return Some(Rect::new(0, 0, i32::MAX / 2, i32::MAX / 2));
        };
        let total = term.rows();
        let visible = layout.visible.clamp(1, total);
        let (cursor_row, cursor_col) = term.cursor();
        let first = first_row(cursor_row, visible, total);
        let rows: Vec<Vec<Cell>> = (0..visible)
            .map(|slot| view_line(term, first, offset, slot).map_or_else(Vec::new, |l| used(l.cells)))
            .collect();
        let spans: Vec<Option<(usize, usize)>> = (0..visible)
            .map(|slot| {
                line_number(term, first, offset, slot).and_then(|line| term.selection_span(line))
            })
            .collect();
        let slot = (cursor_row + offset).saturating_sub(first);
        let cursor = (cursor_row >= first && slot < visible)
            .then(|| (slot, cursor_col, term.cursor_visible()));
        let anchor = (term.screen_top() + first as u64).saturating_sub(offset as u64);
        let bar = (offset > 0).then(|| (max_offset(term, visible), offset));
        let whole = self.layout != Some(layout)
            || self.first != first
            || self.anchor != anchor
            || self.rows.len() != rows.len();
        let mut changed: Option<(usize, usize)> = None;
        let mut mark = |slot: usize| {
            changed = Some(match changed {
                Some((lo, hi)) => (lo.min(slot), hi.max(slot)),
                None => (slot, slot),
            });
        };
        if whole {
            mark(0);
            mark(rows.len().saturating_sub(1));
        } else {
            for (slot, (old, new)) in self.rows.iter().zip(&rows).enumerate() {
                if old != new || self.spans.get(slot) != spans.get(slot) {
                    mark(slot);
                }
            }
            if self.cursor != cursor {
                for (slot, ..) in self.cursor.into_iter().chain(cursor) {
                    mark(slot.min(rows.len().saturating_sub(1)));
                }
            }
        }
        let bar_moved = self.bar != bar;
        self.layout = Some(layout);
        self.first = first;
        self.anchor = anchor;
        self.rows = rows;
        self.spans = spans;
        self.cursor = cursor;
        self.bar = bar;
        let band = changed.map(|(lo, hi)| {
            if whole {
                Rect::new(0, 0, layout.width, layout.height)
            } else {
                layout.rows(lo, hi)
            }
        });
        union(band, bar_moved.then(|| layout.strip()))
    }
}

/// Whether two rectangles share a pixel.
pub fn overlaps(a: Rect, b: Rect) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

/// The pixels two rectangles share (empty when they do not overlap).
pub fn intersect(a: Rect, b: Rect) -> Rect {
    Rect::new(
        a.left.max(b.left),
        a.top.max(b.top),
        a.right.min(b.right),
        a.bottom.min(b.bottom),
    )
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

    #[test]
    fn typing_repaints_only_the_cursor_row() {
        let mut grid = Term::new(80, 24);
        let mut shown = Shown::default();
        grid.feed(b"/ # ");
        assert!(shown.update(&grid, Some(LAYOUT), 0).is_some(), "first frame");
        grid.feed(b"e");
        assert_eq!(
            shown.update(&grid, Some(LAYOUT), 0),
            Some(Rect::new(0, 10, 640, 30))
        );
        assert_eq!(shown.update(&grid, Some(LAYOUT), 0), None, "nothing changed");
    }

    #[test]
    fn a_new_line_repaints_both_rows_and_a_scroll_everything() {
        let mut grid = Term::new(80, 24);
        let mut shown = Shown::default();
        shown.update(&grid, Some(LAYOUT), 0);
        grid.feed(b"out\r\n");
        assert_eq!(
            shown.update(&grid, Some(LAYOUT), 0),
            Some(Rect::new(0, 10, 640, 50))
        );
        for _ in 0..30 {
            grid.feed(b"line\r\n");
        }
        assert_eq!(
            shown.update(&grid, Some(LAYOUT), 0),
            Some(Rect::new(0, 0, 640, 400))
        );
    }

    #[test]
    fn a_new_layout_repaints_everything() {
        let grid = Term::new(80, 24);
        let mut shown = Shown::default();
        shown.update(&grid, Some(LAYOUT), 0);
        let wider = Layout {
            width: 800,
            ..LAYOUT
        };
        assert_eq!(
            shown.update(&grid, Some(wider), 0),
            Some(Rect::new(0, 0, 800, 400))
        );
    }

    #[test]
    fn hiding_the_cursor_repaints_its_row() {
        let mut term = Term::new(80, 24);
        let mut shown = Shown::default();
        term.feed(b"/ # ");
        shown.update(&term, Some(LAYOUT), 0);
        term.feed(b"\x1b[?25l");
        assert_eq!(
            shown.update(&term, Some(LAYOUT), 0),
            Some(Rect::new(0, 10, 640, 30))
        );
    }

    /// A screen of 24 rows with `n` numbered lines written: the window shows
    /// the last 19, the scrollback holds the rest.
    fn filled(n: usize) -> Term {
        let mut term = Term::new(80, 24);
        for line in 1..=n {
            term.feed(format!("line {line}\r\n").as_bytes());
        }
        term
    }

    #[test]
    fn a_scroll_repaints_the_grid_once() {
        let term = filled(100);
        let mut shown = Shown::default();
        shown.update(&term, Some(LAYOUT), 0);
        assert_eq!(
            shown.update(&term, Some(LAYOUT), 3),
            Some(Rect::new(0, 0, 640, 400)),
            "a wheel notch invalidates the grid in one rectangle"
        );
        assert_eq!(shown.update(&term, Some(LAYOUT), 3), None, "and then nothing");
        assert_eq!(
            shown.update(&term, Some(LAYOUT), 0),
            Some(Rect::new(0, 0, 640, 400)),
            "back to the bottom"
        );
    }

    #[test]
    fn output_while_scrolled_back_repaints_only_the_scrollbar() {
        let mut term = filled(100);
        let mut shown = Shown::default();
        shown.update(&term, Some(LAYOUT), 10);
        let (before, top) = (term.scrollback_len(), term.screen_top());
        term.feed(b"more output\r\n");
        // The host anchors the view: the offset grows by the lines added.
        let offset = 10 + (term.screen_top() - top) as usize;
        assert!(term.scrollback_len() >= before);
        assert_eq!(
            shown.update(&term, Some(LAYOUT), offset),
            Some(LAYOUT.strip()),
            "the same text is in view; only the thumb moved"
        );
        assert_eq!(shown.update(&term, Some(LAYOUT), offset), None);
    }

    #[test]
    fn the_scrollbar_appears_and_disappears_with_a_strip_repaint() {
        let term = filled(100);
        let mut shown = Shown::default();
        shown.update(&term, Some(LAYOUT), 0);
        assert!(shown.update(&term, Some(LAYOUT), 1).is_some());
        assert!(shown.update(&term, Some(LAYOUT), 0).is_some());
    }

    #[test]
    fn the_view_reads_history_by_absolute_line() {
        let term = filled(100);
        let first = first_row(term.cursor().0, 19, 24);
        let text = |offset: usize, slot: usize| {
            let line = view_line(&term, first, offset, slot).unwrap();
            line.cells.iter().map(|c| c.ch).collect::<String>().trim_end().to_string()
        };
        assert_eq!(text(0, 17), "line 100");
        assert_eq!(text(5, 17), "line 95");
        assert_eq!(text(5, 0), "line 78");
        let top = max_offset(&term, 19);
        assert_eq!(text(top, 0), "line 1", "the oldest line is reachable");
        assert!(view_line(&term, first, top + 1, 0).is_none());
    }

    #[test]
    fn the_thumb_tracks_the_offset() {
        assert_eq!(thumb(10, 380, 1000, 0, 19, 16), None, "hidden at the bottom");
        let (top, bottom) = thumb(10, 380, 1000, 1000, 19, 16).unwrap();
        assert_eq!(top, 10, "at the oldest line the thumb is at the top");
        assert_eq!(bottom - top, 16, "tiny thumbs keep a minimum size");
        let (top, bottom) = thumb(10, 380, 1000, 1, 19, 16).unwrap();
        assert!((388..=390).contains(&bottom), "one line back it is at the bottom");
        assert!(top < bottom);
        let (a, _) = thumb(10, 380, 1000, 500, 19, 16).unwrap();
        assert!(a > 10 && a < 374, "{a}");
        assert!(thumb(10, 380, 0, 1, 19, 16).is_none());
    }

    #[test]
    fn rectangles() {
        let a = Rect::new(0, 0, 10, 10);
        assert!(overlaps(a, Rect::new(9, 9, 20, 20)));
        assert!(!overlaps(a, Rect::new(10, 0, 20, 10)));
        assert_eq!(
            intersect(a, Rect::new(5, -5, 20, 5)),
            Rect::new(5, 0, 10, 5)
        );
    }
}
