//! Feed panes: staging a page until its rows are shaped, paging, reset.

use super::*;

impl LazySkyApp {
    /// Starts a pane over: answers for what it showed are dropped.
    pub(super) fn reset_pane(&mut self, pane: u8) {
        let state = &mut self.panes[pane as usize];
        state.generation += 1;
        state.staging = None;
        state.focus = None;
        state.source = None;
    }

    /// Asks for a page of `source` for `pane` (a first page when `cursor` is
    /// `None`); "Load more" asks again from the same source.
    pub(super) fn request_feed(&mut self, pane: u8, source: FeedSource, cursor: Option<String>) {
        let generation = self.panes[pane as usize].generation;
        self.panes[pane as usize].source = Some(source.clone());
        let seq = self.seq();
        self.ask(
            seq,
            Request::Feed {
                seq,
                source,
                cursor,
            },
            Route::Feed { pane, generation },
        );
    }

    /// The first page of the timeline, replacing what the home pane shows.
    pub(super) fn refresh_timeline(&mut self) {
        self.reset_pane(HOME);
        self.request_feed(HOME, FeedSource::Timeline, None);
    }

    pub(super) fn stage(
        &mut self,
        pane: u8,
        replace: bool,
        items: Vec<FeedItem>,
        cursor: Option<String>,
    ) {
        let ctx = {
            let s = self.view(pane).state.borrow();
            timeline::context(&s, now_unix())
        };
        let seq = self.seq();
        let staging = Staging::start(&self.layouts, pane, seq, replace, items, cursor, ctx);
        self.panes[pane as usize].staging = Some(staging);
        self.finish_staging_if_ready(pane);
    }

    /// Inserts the staged page once every row of it is shaped.
    pub(super) fn finish_staging_if_ready(&mut self, pane: u8) {
        let ready = self.panes[pane as usize]
            .staging
            .as_ref()
            .is_some_and(Staging::is_complete);
        if !ready {
            return;
        }
        let Some(staging) = self.panes[pane as usize].staging.take() else {
            return;
        };
        let (rows, replace) = staging.into_rows();
        let focus = self.panes[pane as usize].focus.take();
        let total = {
            let mut s = self.view(pane).state.borrow_mut();
            if replace {
                s.feed.replace_all(rows);
                // A refresh asks for the pictures that failed again.
                s.images.borrow_mut().retry_failed();
            } else {
                s.feed.append(rows);
            }
            s.moved = true;
            s.note = if s.feed.is_empty() {
                "Nothing here yet.".into()
            } else {
                String::new()
            };
            if let Some(uri) = focus {
                let at =
                    s.feed.rows().iter().position(
                        |r| matches!(&r.kind, RowKind::Post(item) if item.post.uri == uri),
                    );
                if let Some(i) = at {
                    let top = s.feed.row_top(i);
                    s.feed.set_scroll_top(top);
                }
            }
            s.feed
                .rows()
                .iter()
                .filter(|r| matches!(r.kind, RowKind::Post(_)))
                .count()
        };
        self.images_dirty = true;
        self.view(pane).invalidate();
        if pane == HOME {
            self.home.say("");
            (self.report)(&Msg::Report(Report::FeedRows(total)));
        } else {
            self.detail.say("");
            (self.report)(&Msg::Report(Report::DetailRows(total)));
        }
    }

    pub(super) fn load_more(&mut self, pane: u8) {
        if self.panes[pane as usize].staging.is_some() {
            return;
        }
        let cursor = {
            let s = self.view(pane).state.borrow();
            match s.feed.rows().last().map(|r| &r.kind) {
                Some(RowKind::LoadMore { cursor }) => Some(cursor.clone()),
                _ => None,
            }
        };
        let source = self.panes[pane as usize].source.clone();
        if let (Some(cursor), Some(source)) = (cursor, source) {
            self.say("Loading...");
            self.request_feed(pane, source, Some(cursor));
        }
    }

    pub(super) fn clear_everything(&mut self) {
        for pane in [HOME, DETAIL] {
            self.panes[pane as usize] = PaneState {
                generation: self.panes[pane as usize].generation + 1,
                ..PaneState::default()
            };
            let mut s = self.view(pane).state.borrow_mut();
            s.feed.replace_all(Vec::new());
            s.note = String::new();
            drop(s);
            self.view(pane).invalidate();
        }
        self.routes.clear();
        self.nav = vec![Nav::Timeline];
        self.pictures.clear();
        self.reply = None;
    }
}
