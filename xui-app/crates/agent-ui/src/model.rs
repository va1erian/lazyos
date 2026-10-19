//! The transcript model (docs/ai-agent-plan.md §9.1): rows with final heights,
//! prefix sums, and a scroll position that is **an anchor row and an offset
//! into it**, so a row that grows (a streaming reply) or is laid out again
//! (a resize) does not move what the user is reading. While the view is at
//! the bottom it follows the end ("follow"). No I/O, no xui type: the window
//! instantiates it with its own prepared payload `P`.

use std::ops::Range;
use std::sync::Arc;

/// A row's stable identity: its position in the conversation, or [`LIVE`].
pub type RowId = u64;

/// The row of the reply being streamed.
pub const LIVE: RowId = u64::MAX - 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    User,
    Assistant,
    /// A model's reasoning, collapsed to a line until clicked.
    Thinking {
        open: bool,
    },
    /// An error or an interruption.
    Failure,
    /// A divider or a note (a summary replaced earlier turns).
    Notice,
}

/// What a row says, before it is shaped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowSpec {
    pub kind: RowKind,
    pub text: String,
}

#[derive(Debug)]
pub struct Row<P> {
    pub id: RowId,
    pub spec: Arc<RowSpec>,
    /// Device pixels; an estimate until `prepared` arrives.
    pub height: i32,
    /// What was shaped for this row; `None` while dehydrated or not yet made.
    pub prepared: Option<P>,
    /// `prepared` is for an older `spec` or width and is drawn until a new
    /// one arrives.
    pub dirty: bool,
}

#[derive(Debug)]
pub struct Transcript<P> {
    rows: Vec<Row<P>>,
    /// `tops[i]` is the sum of the heights above row `i`; the last entry is
    /// the total.
    tops: Vec<i32>,
    anchor: (RowId, i32),
    view_h: i32,
    follow: bool,
}

impl<P> Default for Transcript<P> {
    fn default() -> Transcript<P> {
        Transcript::new()
    }
}

/// A guess at a row's height before it is shaped, in device pixels.
pub fn estimate(spec: &RowSpec, width: i32, line: i32) -> i32 {
    let per_line = (width.max(80) / 7).max(10) as usize;
    let lines: usize = spec
        .text
        .split('\n')
        .map(|l| 1 + l.chars().count() / per_line)
        .sum();
    (lines.min(400) as i32 + 1) * line + line
}

impl<P> Transcript<P> {
    pub fn new() -> Transcript<P> {
        Transcript {
            rows: Vec::new(),
            tops: vec![0],
            anchor: (0, 0),
            view_h: 0,
            follow: true,
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn rows(&self) -> &[Row<P>] {
        &self.rows
    }

    pub fn row(&self, i: usize) -> Option<&Row<P>> {
        self.rows.get(i)
    }

    pub fn row_mut(&mut self, i: usize) -> Option<&mut Row<P>> {
        self.rows.get_mut(i)
    }

    pub fn index_of(&self, id: RowId) -> Option<usize> {
        self.rows.iter().position(|r| r.id == id)
    }

    pub fn total_height(&self) -> i32 {
        *self.tops.last().unwrap_or(&0)
    }

    pub fn view_height(&self) -> i32 {
        self.view_h
    }

    pub fn max_scroll(&self) -> i32 {
        (self.total_height() - self.view_h).max(0)
    }

    /// The view follows the end of the transcript.
    pub fn following(&self) -> bool {
        self.follow
    }

    /// The absolute scroll position, derived from the anchor.
    pub fn scroll_top(&self) -> i32 {
        match self.index_of(self.anchor.0) {
            Some(i) => (self.tops[i] + self.anchor.1).clamp(0, self.max_scroll()),
            None => 0,
        }
    }

    pub fn row_top(&self, i: usize) -> i32 {
        self.tops.get(i).copied().unwrap_or(self.total_height())
    }

    pub fn screen_top(&self, i: usize) -> i32 {
        self.row_top(i) - self.scroll_top()
    }

    /// The row containing content position `y`.
    pub fn row_at(&self, y: i32) -> Option<usize> {
        if self.rows.is_empty() {
            return None;
        }
        let after = self.tops[1..].partition_point(|&bottom| bottom <= y);
        Some(after.min(self.rows.len() - 1))
    }

    /// The rows intersecting the view.
    pub fn visible(&self) -> Range<usize> {
        let Some(first) = self.row_at(self.scroll_top()) else {
            return 0..0;
        };
        let bottom = self.scroll_top() + self.view_h;
        let end = self.tops[1..].partition_point(|&b| b < bottom) + 1;
        first..end.min(self.rows.len())
    }

    /// The rows within `screens` view heights of the view.
    pub fn near(&self, screens: i32) -> Range<usize> {
        let margin = self.view_h.max(1) * screens;
        let top = (self.scroll_top() - margin).max(0);
        let bottom = self.scroll_top() + self.view_h + margin;
        let Some(first) = self.row_at(top) else {
            return 0..0;
        };
        let end = self.tops[1..].partition_point(|&b| b < bottom) + 1;
        first..end.min(self.rows.len())
    }

    pub fn set_view_height(&mut self, h: i32) {
        self.view_h = h.max(0);
        self.settle();
    }

    /// Scrolls to `y` (clamped); following resumes at the bottom.
    pub fn set_scroll_top(&mut self, y: i32) {
        if self.rows.is_empty() {
            self.anchor = (0, 0);
            self.follow = true;
            return;
        }
        let y = y.clamp(0, self.max_scroll());
        let i = self.row_at(y).unwrap_or(0);
        self.anchor = (self.rows[i].id, y - self.tops[i]);
        self.follow = y >= self.max_scroll() - 2;
    }

    pub fn scroll_by(&mut self, dy: i32) {
        self.set_scroll_top(self.scroll_top().saturating_add(dy));
    }

    pub fn scroll_to_end(&mut self) {
        self.set_scroll_top(self.max_scroll());
        self.follow = true;
    }

    /// After a change: while following, the view stays on the end.
    fn settle(&mut self) {
        if self.follow {
            self.scroll_to_end();
        } else {
            let y = self.scroll_top();
            self.set_scroll_top(y);
        }
    }

    /// Replaces the rows with `next` (`(id, spec, estimated height)`), keeping
    /// what was shaped for an unchanged row and drawing the old shape of a
    /// changed one until a new one arrives. The anchor row keeps its place.
    pub fn replace(&mut self, next: Vec<(RowId, Arc<RowSpec>, i32)>) {
        let follow = self.follow;
        let anchor = self.anchor;
        let mut old: Vec<Option<Row<P>>> = std::mem::take(&mut self.rows)
            .into_iter()
            .map(Some)
            .collect();
        let mut rows = Vec::with_capacity(next.len());
        for (id, spec, guess) in next {
            let found = old
                .iter_mut()
                .find(|r| r.as_ref().is_some_and(|r| r.id == id))
                .and_then(Option::take);
            rows.push(match found {
                Some(mut row) => {
                    if *row.spec != *spec {
                        row.spec = spec;
                        row.dirty = true;
                    }
                    row
                }
                None => Row {
                    id,
                    spec,
                    height: guess.max(1),
                    prepared: None,
                    dirty: false,
                },
            });
        }
        self.rows = rows;
        self.rebuild();
        self.anchor = anchor;
        self.follow = follow;
        if !self.rows.iter().any(|r| r.id == anchor.0) {
            self.anchor = self.rows.first().map_or((0, 0), |r| (r.id, 0));
        }
        self.settle();
    }

    /// Gives rows their real heights in one pass; the anchor row keeps its
    /// place on screen.
    pub fn set_heights(&mut self, heights: &[(RowId, i32)]) {
        let (anchor, follow) = (self.anchor, self.follow);
        for &(id, h) in heights {
            if let Some(i) = self.index_of(id) {
                self.rows[i].height = h.max(1);
            }
        }
        self.rebuild();
        self.anchor = anchor;
        self.follow = follow;
        if let Some(i) = self.index_of(anchor.0) {
            self.anchor.1 = anchor.1.clamp(0, self.rows[i].height - 1);
        }
        self.settle();
    }

    fn rebuild(&mut self) {
        self.tops.clear();
        self.tops.reserve(self.rows.len() + 1);
        let mut top = 0i32;
        for row in &self.rows {
            self.tops.push(top);
            top = top.saturating_add(row.height);
        }
        self.tops.push(top);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(text: &str) -> Arc<RowSpec> {
        Arc::new(RowSpec {
            kind: RowKind::Assistant,
            text: text.into(),
        })
    }

    fn rows(n: u64, h: i32) -> Vec<(RowId, Arc<RowSpec>, i32)> {
        (0..n).map(|i| (i, spec(&format!("row {i}")), h)).collect()
    }

    fn model(n: u64) -> Transcript<()> {
        let mut t = Transcript::new();
        t.set_view_height(200);
        t.replace(rows(n, 100));
        t
    }

    #[test]
    fn a_short_transcript_has_nothing_to_scroll_and_follows() {
        let t = model(1);
        assert_eq!(t.max_scroll(), 0);
        assert!(t.following());
    }

    #[test]
    fn following_sticks_to_the_end_as_rows_arrive_and_grow() {
        let mut t = model(5);
        assert_eq!(t.scroll_top(), 300);
        t.replace(rows(6, 100));
        assert_eq!(t.scroll_top(), 400, "a new row keeps the end in view");
        t.set_heights(&[(5, 250)]);
        assert_eq!(t.scroll_top(), t.max_scroll(), "a growing last row too");
    }

    #[test]
    fn scrolling_up_stops_following_and_the_anchor_holds_still() {
        let mut t = model(10);
        t.scroll_by(-250);
        assert!(!t.following());
        let (top, row) = (t.scroll_top(), t.row_at(t.scroll_top()).unwrap());
        // A row above grows: the row at the top of the view stays at the top.
        t.set_heights(&[(0, 180)]);
        assert_eq!(t.row_at(t.scroll_top()).unwrap(), row);
        assert_ne!(t.scroll_top(), top);
        assert_eq!(t.screen_top(row), -t.anchor.1);
        // New rows at the end do not drag the view down.
        let before = t.scroll_top();
        t.replace(
            rows(12, 100)
                .into_iter()
                .map(|(i, s, h)| (i, s, if i == 0 { 180 } else { h }))
                .collect(),
        );
        assert_eq!(t.scroll_top(), before);
        t.scroll_to_end();
        assert!(t.following());
    }

    #[test]
    fn an_unchanged_row_keeps_its_shape_and_a_changed_one_is_dirty() {
        let mut t: Transcript<u32> = Transcript::new();
        t.set_view_height(100);
        t.replace(vec![(0, spec("a"), 50), (1, spec("b"), 50)]);
        t.row_mut(0).unwrap().prepared = Some(7);
        t.row_mut(1).unwrap().prepared = Some(8);
        t.replace(vec![
            (0, spec("a"), 50),
            (1, spec("b changed"), 50),
            (2, spec("c"), 50),
        ]);
        assert_eq!(t.row(0).unwrap().prepared, Some(7));
        assert!(!t.row(0).unwrap().dirty);
        assert_eq!(t.row(1).unwrap().prepared, Some(8));
        assert!(t.row(1).unwrap().dirty);
        assert_eq!(t.row(2).unwrap().prepared, None);
    }

    #[test]
    fn visible_and_hit_testing_follow_the_prefix_sums() {
        let mut t = model(10);
        t.set_scroll_top(450);
        assert_eq!(t.visible(), 4..7);
        assert_eq!(t.row_at(450), Some(4));
        assert_eq!(t.screen_top(4), -50);
        assert_eq!(t.row_at(10_000), Some(9));
        assert!(t.near(1).start <= 2 && t.near(1).end >= 9);
        t.replace(Vec::new());
        assert_eq!(t.visible(), 0..0);
        assert_eq!(t.row_at(0), None);
    }

    #[test]
    fn the_estimate_grows_with_the_text() {
        let short = estimate(&spec("hi"), 600, 20);
        let long = estimate(&spec(&"word ".repeat(400)), 600, 20);
        assert!(long > short * 3, "{short} {long}");
    }
}
