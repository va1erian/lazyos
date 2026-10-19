//! Keeping the transcript view, the layout thread and the conversation list
//! in step with the app's state.

use super::*;
use crate::layouts::Job;
use crate::model::{estimate, RowSpec};
use crate::prepare::Context;
use crate::rows;

/// Layout results applied per tick: more wait for the next one.
const APPLY_PER_TICK: usize = 24;
/// How many screens around the view are kept shaped.
const NEAR_SCREENS: i32 = 2;

impl LazyAgentApp {
    /// Rebuilds the rows from the conversation, the live reply and the
    /// failure; what is unchanged keeps its shape.
    pub(super) fn rebuild_rows(&mut self) {
        self.rows_dirty = false;
        let specs = rows::specs(
            self.current.as_deref(),
            &self.live,
            self.pending_user.as_deref(),
            self.failure.as_deref(),
            &self.opened,
        );
        let count = specs.len();
        let Ok(mut state) = self.chat.view.state.try_borrow_mut() else {
            // A paint or an event holds the view: the next tick tries again.
            self.rows_dirty = true;
            return;
        };
        let (width, line) = (state.content_width(), state.scaled(20));
        let next: Vec<(RowId, Arc<RowSpec>, i32)> = specs
            .into_iter()
            .map(|(id, spec)| {
                let guess = estimate(&spec, width, line);
                (id, spec, guess)
            })
            .collect();
        state.transcript.replace(next);
        state.moved = true;
        drop(state);
        self.chat.view.invalidate();
        if count != self.reported_rows {
            self.reported_rows = count;
            self.say(Report::Rows(count));
        }
    }

    /// Applies finished layouts: the row shows its new shape and height.
    pub(super) fn apply_layouts(&mut self) {
        let mut done = Vec::new();
        while done.len() < APPLY_PER_TICK {
            let Some(next) = self.layouts.try_next() else {
                break;
            };
            done.push(next);
        }
        if done.is_empty() {
            return;
        }
        let Ok(mut state) = self.chat.view.state.try_borrow_mut() else {
            // Never lose a result to a held borrow: they are applied next tick.
            for d in done {
                self.carried.push(d);
            }
            return;
        };
        let mut heights = Vec::new();
        for d in self.carried.drain(..).chain(done) {
            let wanted = self.submitted.get(&d.id).map(|(rev, _, _)| *rev);
            if wanted != Some(d.rev) {
                continue;
            }
            if let Some(i) = state.transcript.index_of(d.id) {
                heights.push((d.id, d.prepared.height));
                if let Some(row) = state.transcript.row_mut(i) {
                    row.prepared = Some(d.prepared);
                    row.dirty = false;
                }
            }
        }
        state.transcript.set_heights(&heights);
        drop(state);
        self.chat.view.invalidate();
    }

    /// Asks for the rows near the view that are missing, out of date or laid
    /// out for another width.
    pub(super) fn maintain_layouts(&mut self) {
        let Ok(mut state) = self.chat.view.state.try_borrow_mut() else {
            return;
        };
        let width = state.content_width();
        let dpi = state.dpi;
        if state.relayout {
            state.relayout = false;
            for i in 0..state.transcript.len() {
                if let Some(row) = state.transcript.row_mut(i) {
                    row.dirty = true;
                }
            }
        }
        state.moved = false;
        let near = state.transcript.near(NEAR_SCREENS);
        let visible = state.transcript.visible();
        let mut jobs = Vec::new();
        // The visible rows first, then the rest of the neighbourhood.
        let order: Vec<usize> = visible
            .clone()
            .chain(near.filter(|i| !visible.contains(i)))
            .collect();
        for i in order {
            let Some(row) = state.transcript.row(i) else {
                continue;
            };
            let stale = row.prepared.is_none()
                || row.dirty
                || row.prepared.as_ref().is_some_and(|p| p.width != width);
            let identity = Arc::as_ptr(&row.spec) as usize;
            let current = self
                .submitted
                .get(&row.id)
                .is_some_and(|(_, w, ptr)| *w == width && *ptr == identity);
            if stale && !current {
                self.rev += 1;
                self.submitted.insert(row.id, (self.rev, width, identity));
                jobs.push(Job {
                    id: row.id,
                    spec: Arc::clone(&row.spec),
                    ctx: Context { width, dpi },
                    rev: self.rev,
                });
            }
        }
        drop(state);
        if !jobs.is_empty() {
            self.layouts.submit(jobs);
        }
    }

    /// Shows the conversations (or the search hits) in the sidebar.
    pub(super) fn refresh_list(&mut self) {
        let (ids, titles): (Vec<String>, Vec<String>) = match &self.hits {
            Some(hits) => hits
                .iter()
                .map(|h| (h.id.clone(), format!("{} - {}", h.title, h.snippet)))
                .unzip(),
            None => self
                .conversations
                .iter()
                .map(|c| (c.id.clone(), c.title.clone()))
                .unzip(),
        };
        self.shown = ids;
        self.chat.show_list(&titles);
    }
}
