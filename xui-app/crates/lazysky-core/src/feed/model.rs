//! The feed model (docs/lazysky-plan.md §6.1, R3): rows with final heights,
//! prefix sums, and a scroll position that is **an anchor row and an offset
//! into it**, never a raw pixel offset.
//!
//! The model is generic over the payload `P` a front end hangs on a row (the
//! UI's shaped text); it has no I/O and no xui type. A change of height or of
//! the rows above the viewport (a refresh prepending, a resize re-layout, a
//! deleted post) keeps the anchor row where it is on screen, because the
//! absolute position is derived from the anchor after every change.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use crate::proto::FeedItem;

/// A row's stable identity (a hash of the post URI and the reposter, see
/// [`FeedItem::row_id`]).
pub type RowId = u64;

/// The id of the "Load more" row at the end of a feed.
pub const LOAD_MORE_ID: RowId = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    Post(Arc<FeedItem>),
    /// The row that asks for the next page, with the cursor it will use.
    LoadMore {
        cursor: String,
    },
}

#[derive(Debug, Clone)]
pub struct FeedRow<P> {
    pub id: RowId,
    /// Device pixels; final once the row is in the feed (R2).
    pub height: i32,
    pub kind: RowKind,
    /// What the front end shaped for this row; `None` while dehydrated.
    pub prepared: Option<P>,
    /// `prepared` is out of date (the post changed under it) and is drawn
    /// until a new one arrives.
    pub dirty: bool,
}

impl<P> FeedRow<P> {
    pub fn post(item: Arc<FeedItem>, height: i32, prepared: Option<P>) -> FeedRow<P> {
        FeedRow {
            id: item.row_id(),
            height: height.max(1),
            kind: RowKind::Post(item),
            prepared,
            dirty: false,
        }
    }

    pub fn load_more(cursor: String, height: i32) -> FeedRow<P> {
        FeedRow {
            id: LOAD_MORE_ID,
            height: height.max(1),
            kind: RowKind::LoadMore { cursor },
            prepared: None,
            dirty: false,
        }
    }
}

#[derive(Debug)]
pub struct Feed<P> {
    rows: Vec<FeedRow<P>>,
    /// `tops[i]` is the sum of the heights above row `i`; the last entry is
    /// the total, so `tops.len() == rows.len() + 1`.
    tops: Vec<i32>,
    index: HashMap<RowId, usize>,
    /// The row at the top of the view and how far into it the view starts.
    anchor: (RowId, i32),
    view_h: i32,
}

impl<P> Default for Feed<P> {
    fn default() -> Feed<P> {
        Feed::new()
    }
}

impl<P> Feed<P> {
    pub fn new() -> Feed<P> {
        Feed {
            rows: Vec::new(),
            tops: vec![0],
            index: HashMap::new(),
            anchor: (LOAD_MORE_ID, 0),
            view_h: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn rows(&self) -> &[FeedRow<P>] {
        &self.rows
    }

    pub fn row(&self, i: usize) -> Option<&FeedRow<P>> {
        self.rows.get(i)
    }

    pub fn row_mut(&mut self, i: usize) -> Option<&mut FeedRow<P>> {
        self.rows.get_mut(i)
    }

    pub fn index_of(&self, id: RowId) -> Option<usize> {
        self.index.get(&id).copied()
    }

    pub fn total_height(&self) -> i32 {
        *self.tops.last().unwrap_or(&0)
    }

    pub fn view_height(&self) -> i32 {
        self.view_h
    }

    /// The anchor: the row at the top of the view and the offset into it.
    pub fn anchor(&self) -> (RowId, i32) {
        self.anchor
    }

    /// The largest scroll position.
    pub fn max_scroll(&self) -> i32 {
        (self.total_height() - self.view_h).max(0)
    }

    /// The absolute scroll position, derived from the anchor.
    pub fn scroll_top(&self) -> i32 {
        let Some(&i) = self.index.get(&self.anchor.0) else {
            return 0;
        };
        (self.tops[i] + self.anchor.1).clamp(0, self.max_scroll())
    }

    /// The top of row `i` in the content, in pixels.
    pub fn row_top(&self, i: usize) -> i32 {
        self.tops.get(i).copied().unwrap_or(self.total_height())
    }

    /// The top of row `i` relative to the view's top (negative when it is
    /// partly above).
    pub fn screen_top(&self, i: usize) -> i32 {
        self.row_top(i) - self.scroll_top()
    }

    /// The row containing content position `y` (the last row for a `y` past
    /// the end; `None` when the feed is empty).
    pub fn row_at(&self, y: i32) -> Option<usize> {
        if self.rows.is_empty() {
            return None;
        }
        let after = self.tops[1..].partition_point(|&bottom| bottom <= y);
        Some(after.min(self.rows.len() - 1))
    }

    /// The rows intersecting the view, as an index range.
    pub fn visible(&self) -> Range<usize> {
        let Some(first) = self.row_at(self.scroll_top()) else {
            return 0..0;
        };
        let bottom = self.scroll_top() + self.view_h;
        let end = self.tops[1..].partition_point(|&row_bottom| row_bottom < bottom) + 1;
        first..end.min(self.rows.len())
    }

    /// The rows within `screens` view heights of the view: the ones worth
    /// keeping prepared (§6.4).
    pub fn near(&self, screens: i32) -> Range<usize> {
        let margin = self.view_h.max(1) * screens;
        let top = (self.scroll_top() - margin).max(0);
        let bottom = self.scroll_top() + self.view_h + margin;
        let Some(first) = self.row_at(top) else {
            return 0..0;
        };
        let end = self.tops[1..].partition_point(|&row_bottom| row_bottom < bottom) + 1;
        first..end.min(self.rows.len())
    }

    /// Sets the view's height and keeps the position valid.
    pub fn set_view_height(&mut self, h: i32) {
        self.view_h = h.max(0);
        self.set_scroll_top(self.scroll_top());
    }

    /// Scrolls to the absolute position `y` (clamped).
    pub fn set_scroll_top(&mut self, y: i32) {
        if self.rows.is_empty() {
            self.anchor = (LOAD_MORE_ID, 0);
            return;
        }
        let y = y.clamp(0, self.max_scroll());
        let i = self.row_at(y).unwrap_or(0);
        self.anchor = (self.rows[i].id, y - self.tops[i]);
    }

    /// Scrolls by `dy` pixels.
    pub fn scroll_by(&mut self, dy: i32) {
        self.set_scroll_top(self.scroll_top().saturating_add(dy));
    }

    /// `rows` without those whose id is already in the feed or earlier in
    /// `rows` (a post the next page repeats): ids are the model's keys.
    fn fresh(&self, rows: Vec<FeedRow<P>>) -> Vec<FeedRow<P>> {
        let mut seen = std::collections::HashSet::new();
        rows.into_iter()
            .filter(|row| !self.index.contains_key(&row.id) && seen.insert(row.id))
            .collect()
    }

    /// Replaces every row; the view goes to the top.
    pub fn replace_all(&mut self, rows: Vec<FeedRow<P>>) {
        self.rows.clear();
        self.index.clear();
        let rows = self.fresh(rows);
        self.rows = rows;
        self.rebuild();
        self.anchor = self.rows.first().map_or((LOAD_MORE_ID, 0), |r| (r.id, 0));
    }

    /// Adds rows at the end. A trailing "Load more" row is dropped first: it
    /// is replaced by what the page brings.
    pub fn append(&mut self, rows: Vec<FeedRow<P>>) {
        self.drop_load_more();
        let was_empty = self.rows.is_empty();
        let rows = self.fresh(rows);
        self.rows.extend(rows);
        self.rebuild();
        if was_empty {
            self.anchor = self.rows.first().map_or((LOAD_MORE_ID, 0), |r| (r.id, 0));
        }
    }

    /// Adds rows at the top without moving what is on screen: the anchor row
    /// keeps its screen position (R3).
    pub fn prepend(&mut self, rows: Vec<FeedRow<P>>) {
        let anchor = self.anchor;
        let was_empty = self.rows.is_empty();
        let mut all = self.fresh(rows);
        all.append(&mut self.rows);
        self.rows = all;
        self.rebuild();
        if was_empty {
            self.anchor = self.rows.first().map_or((LOAD_MORE_ID, 0), |r| (r.id, 0));
        } else {
            self.anchor = anchor;
        }
    }

    /// Removes the row `id` (a deleted post). If the view's anchor was it,
    /// the view starts at the row that took its place.
    pub fn remove(&mut self, id: RowId) {
        let Some(at) = self.index_of(id) else {
            return;
        };
        let anchored_here = self.anchor.0 == id;
        self.rows.remove(at);
        self.rebuild();
        if anchored_here {
            let next = at.min(self.rows.len().saturating_sub(1));
            self.anchor = self.rows.get(next).map_or((LOAD_MORE_ID, 0), |r| (r.id, 0));
        }
    }

    /// Gives rows new heights (a width change re-laying them out), in one
    /// pass. The anchor row keeps its screen position (R3); the offset into
    /// it is kept inside its new height.
    pub fn set_heights(&mut self, heights: &[(RowId, i32)]) {
        let before = self.anchor;
        for &(id, height) in heights {
            if let Some(&i) = self.index.get(&id) {
                self.rows[i].height = height.max(1);
            }
        }
        self.rebuild();
        self.anchor = before;
        if let Some(&i) = self.index.get(&before.0) {
            let h = self.rows[i].height;
            self.anchor.1 = before.1.clamp(0, h - 1);
        }
    }

    fn drop_load_more(&mut self) {
        if matches!(
            self.rows.last().map(|r| &r.kind),
            Some(RowKind::LoadMore { .. })
        ) {
            let anchored_there = self.anchor.0 == LOAD_MORE_ID;
            self.rows.pop();
            if anchored_there {
                // The view sat on the button: it stays at the old end.
                self.rebuild();
                let end = self.total_height();
                self.set_scroll_top(end);
            }
        }
    }

    /// Recomputes the prefix sums and the id index, and re-validates the
    /// anchor (an anchor whose row is gone goes to the first row).
    fn rebuild(&mut self) {
        self.tops.clear();
        self.tops.reserve(self.rows.len() + 1);
        self.index.clear();
        let mut top = 0i32;
        for (i, row) in self.rows.iter().enumerate() {
            self.tops.push(top);
            top = top.saturating_add(row.height);
            self.index.insert(row.id, i);
        }
        self.tops.push(top);
        if !self.index.contains_key(&self.anchor.0) {
            self.anchor = self.rows.first().map_or((LOAD_MORE_ID, 0), |r| (r.id, 0));
        }
    }
}
