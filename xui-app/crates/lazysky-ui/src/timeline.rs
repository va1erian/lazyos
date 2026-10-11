//! Getting rows into a FeedView prepared (R2, R6): a page is staged until
//! the layout thread has shaped every row, then inserted at once; rows far
//! from the view are dehydrated and come back nearest first.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lazysky_core::feed::{FeedRow, RowId, RowKind, LOAD_MORE_ID};
use lazysky_core::proto::FeedItem;
use lazysky_core::ImageKey;

use crate::feedview::ViewState;
use crate::layouts::{Done, Job, Layouts, Work};
use crate::prepare::{Context, Prepared, DEFAULT_MAX_IMAGE_H};

/// Rows farther than this many view heights from the view are dehydrated
/// (§6.4); rows within [`REPREPARE`] are kept (or made) ready.
const DEHYDRATE: i32 = 3;
const REPREPARE: i32 = 2;
/// How many pictures one list asks the core for.
const MAX_WANTED: usize = 64;

/// A page waiting for its rows to be shaped.
pub struct Staging {
    pub pane: u8,
    pub seq: u64,
    pub replace: bool,
    pub cursor: Option<String>,
    items: Vec<Arc<FeedItem>>,
    done: Vec<Option<Prepared>>,
    load_more: Option<Prepared>,
    index: HashMap<RowId, usize>,
    remaining: usize,
}

impl Staging {
    /// Stages `items` (the answer to request `seq`, for `pane`) and sends
    /// its rows to the layout thread. `cursor` is what "Load more" would use.
    pub fn start(
        layouts: &Layouts,
        pane: u8,
        seq: u64,
        replace: bool,
        items: Vec<FeedItem>,
        cursor: Option<String>,
        ctx: Context,
    ) -> Staging {
        let mut unique = Vec::new();
        let mut index = HashMap::new();
        for item in items {
            let id = item.row_id();
            if index.contains_key(&id) {
                continue;
            }
            index.insert(id, unique.len());
            unique.push(Arc::new(item));
        }
        let mut jobs: Vec<Job> = unique
            .iter()
            .map(|item| Job {
                pane,
                id: item.row_id(),
                work: Work::Post(Arc::clone(item)),
                ctx,
                tag: seq,
            })
            .collect();
        if cursor.is_some() {
            jobs.push(Job {
                pane,
                id: LOAD_MORE_ID,
                work: Work::LoadMore,
                ctx,
                tag: seq,
            });
        }
        let remaining = jobs.len();
        layouts.submit(jobs);
        Staging {
            pane,
            seq,
            replace,
            cursor,
            done: unique.iter().map(|_| None).collect(),
            items: unique,
            load_more: None,
            index,
            remaining,
        }
    }

    /// Takes a finished row. `true` when it was this page's.
    pub fn accept(&mut self, done: Done) -> bool {
        if !done.urgent || done.tag != self.seq || done.pane != self.pane {
            return false;
        }
        if done.id == LOAD_MORE_ID {
            self.load_more = Some(done.prepared);
        } else if let Some(&i) = self.index.get(&done.id) {
            self.done[i] = Some(done.prepared);
        } else {
            return false;
        }
        self.remaining = self.remaining.saturating_sub(1);
        true
    }

    pub fn is_complete(&self) -> bool {
        self.remaining == 0
    }

    /// The page's rows, with their "Load more" row when there is more.
    pub fn into_rows(self) -> (Vec<FeedRow<Prepared>>, bool) {
        let mut rows: Vec<FeedRow<Prepared>> = self
            .items
            .into_iter()
            .zip(self.done)
            .filter_map(|(item, prepared)| {
                let prepared = prepared?;
                Some(FeedRow::post(item, prepared.height, Some(prepared)))
            })
            .collect();
        if let (Some(cursor), Some(prepared)) = (self.cursor, self.load_more) {
            let height = prepared.height;
            let mut row = FeedRow::load_more(cursor, height);
            row.prepared = Some(prepared);
            rows.push(row);
        }
        (rows, self.replace)
    }
}

/// The context for jobs at the view's current size.
pub fn context(state: &ViewState, now: u64) -> Context {
    Context {
        width: state.content_width(),
        dpi: state.dpi,
        now,
        max_image_h: DEFAULT_MAX_IMAGE_H,
    }
}

/// Keeps the rows near the view prepared at the right width and drops the
/// shaped text of rows far away. Returns how many jobs it queued.
pub fn maintain(state: &mut ViewState, layouts: &Layouts, now: u64, pane: u8) -> usize {
    state.moved = false;
    state.relayout = false;
    if state.feed.is_empty() || state.height <= 0 {
        return 0;
    }
    let width = state.content_width();
    let ctx = context(state, now);
    let keep = state.feed.near(DEHYDRATE);
    for i in 0..state.feed.len() {
        if keep.contains(&i) {
            continue;
        }
        if let Some(row) = state.feed.row_mut(i) {
            row.prepared = None;
        }
    }
    let near = state.feed.near(REPREPARE);
    let centre = state.feed.scroll_top() + state.height / 2;
    let mut wanted: Vec<(i32, Job)> = Vec::new();
    for i in near {
        let Some(row) = state.feed.row(i) else {
            continue;
        };
        let fresh = !row.dirty && row.prepared.as_ref().is_some_and(|p| p.width == width);
        if fresh {
            continue;
        }
        let work = match &row.kind {
            RowKind::Post(item) => Work::Post(Arc::clone(item)),
            RowKind::LoadMore { .. } => Work::LoadMore,
        };
        let distance = (state.feed.row_top(i) + row.height / 2 - centre).abs();
        wanted.push((
            distance,
            Job {
                pane,
                id: row.id,
                work,
                ctx,
                tag: 0,
            },
        ));
    }
    wanted.sort_by_key(|(distance, _)| *distance);
    let count = wanted.len();
    layouts.set_refresh(wanted.into_iter().map(|(_, job)| job).collect());
    count
}

/// Applies re-prepared rows (a refresh result): only those still current for
/// the view's width. Returns whether anything changed.
pub fn apply_refresh(state: &mut ViewState, done: Vec<Done>) -> bool {
    let width = state.content_width();
    let mut heights = Vec::with_capacity(done.len());
    let mut changed = false;
    for d in done {
        if d.prepared.width != width {
            // Laid out for an older width: maintenance asks again.
            state.relayout = true;
            continue;
        }
        let Some(i) = state.feed.index_of(d.id) else {
            continue;
        };
        if let Some(row) = state.feed.row_mut(i) {
            heights.push((d.id, d.prepared.height));
            row.prepared = Some(d.prepared);
            row.dirty = false;
            changed = true;
        }
    }
    if !heights.is_empty() {
        state.feed.set_heights(&heights);
    }
    changed
}

/// The pictures to ask for, nearest the view's centre first (those of the
/// rows near the view that are neither held nor known to have failed), and
/// the set the view shows right now (the cache must keep these).
pub fn wanted_images(state: &ViewState) -> (Vec<ImageKey>, HashSet<ImageKey>) {
    let cache = state.images.borrow();
    let centre = state.feed.scroll_top() + state.height / 2;
    let mut rows: Vec<(i32, usize)> = state
        .feed
        .near(REPREPARE)
        .map(|i| {
            let row_h = state.feed.row(i).map_or(0, |r| r.height);
            ((state.feed.row_top(i) + row_h / 2 - centre).abs(), i)
        })
        .collect();
    rows.sort_unstable();
    let mut wanted = Vec::new();
    let mut seen = HashSet::new();
    for (_, i) in rows {
        let Some(prepared) = state.feed.row(i).and_then(|r| r.prepared.as_ref()) else {
            continue;
        };
        for key in prepared.image_keys() {
            if cache.wants(key) && seen.insert(key.clone()) {
                wanted.push(key.clone());
            }
        }
        if wanted.len() >= MAX_WANTED {
            break;
        }
    }
    let visible = state
        .feed
        .visible()
        .filter_map(|i| state.feed.row(i)?.prepared.as_ref())
        .flat_map(|p| p.image_keys().cloned())
        .collect();
    (wanted, visible)
}
