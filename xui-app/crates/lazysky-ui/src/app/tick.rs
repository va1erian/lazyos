//! The timer tick: draining the core, the layout thread and the pictures.

use super::*;

impl LazySkyApp {
    /// One timer tick: drain what finished, keep the rows near the view
    /// prepared, report timings.
    pub(super) fn tick(&mut self, ui: &mut Ui<Msg>) {
        let started = Instant::now();
        self.poll_unread(started);
        self.viewer_request();
        let drained = drain::drain(&self.events, drain::BUDGET);
        for event in drained.events {
            self.on_event(ui, event);
        }
        // Layout results: a staged page's rows complete it; the rest
        // re-prepare rows already in a feed.
        let mut refreshed: [Vec<crate::layouts::Done>; 2] = std::mem::take(&mut self.carried);
        let mut taken = 0;
        while taken < REFRESH_PER_TICK {
            let Some(done) = self.layouts.try_next() else {
                break;
            };
            taken += 1;
            let pane = usize::from(done.pane.min(1));
            if let Some(staging) = self.panes[pane].staging.as_mut() {
                if done.urgent && staging.seq == done.tag {
                    staging.accept(done);
                    continue;
                }
            }
            if !done.urgent {
                refreshed[pane].push(done);
            }
        }
        for pane in [HOME, DETAIL] {
            self.finish_staging_if_ready(pane);
        }
        let mut changed = false;
        let mut pictures = None;
        let mut failures: Vec<String> = Vec::new();
        let mut ready_ms: Option<u64> = None;
        let active = self.active_pane();
        for pane in [HOME, DETAIL] {
            let done = std::mem::take(&mut refreshed[pane as usize]);
            let view = Rc::clone(self.view(pane));
            let Ok(mut s) = view.state.try_borrow_mut() else {
                // An event or a paint holds the view right now (a panic here
                // would abort the app): the same results are applied next tick.
                self.carried[pane as usize] = done;
                continue;
            };
            let mut pane_changed = false;
            if !done.is_empty() {
                pane_changed = timeline::apply_refresh(&mut s, done);
                self.images_dirty |= pane_changed;
            }
            if Some(pane) == active && (s.moved || s.relayout) {
                timeline::maintain(&mut s, &self.layouts, now_unix(), pane);
                self.images_dirty = true;
            }
            drop(s);
            if pane_changed {
                self.view(pane).invalidate();
            }
            changed |= pane_changed;
        }
        // Pictures that arrived become drawable, a few per tick (R5).
        {
            let mut cache = self.images.borrow_mut();
            for _ in 0..IMAGES_PER_TICK {
                let Some((key, result)) = self.image_inbox.pop_front() else {
                    break;
                };
                match result {
                    Ok(bitmap) => {
                        let viewer = self.viewer.state.borrow();
                        if viewer.requested.as_ref() == Some(&key) {
                            if let Some(at) = viewer.asked_at {
                                ready_ms = Some(at.elapsed().as_millis() as u64);
                            }
                        }
                        drop(viewer);
                        cache.insert(key, bitmap);
                    }
                    Err(why) => {
                        failures.push(format!("{}x{} {why} {}", key.width, key.height, key.url));
                        cache.fail(key);
                    }
                }
                changed = true;
                pictures = Some(cache.len());
            }
        }
        if std::mem::take(&mut self.images_dirty) {
            if let Some(pane) = active {
                let s = self.view(pane).state.borrow();
                let (wanted, visible) = timeline::wanted_images(&s);
                drop(s);
                self.core.want_images(wanted);
                let protect: HashSet<&ImageKey> = visible.iter().collect();
                self.images.borrow_mut().evict(&protect);
            }
        }
        if let Some(ms) = ready_ms {
            (self.report)(&Msg::Report(Report::ViewerReady(ms)));
        }
        for failure in failures {
            (self.report)(&Msg::Report(Report::ImageFailed(failure)));
        }
        if let Some(count) = pictures {
            (self.report)(&Msg::Report(Report::Pictures(count)));
        }
        if let Some(pane) = active {
            self.view(pane).autoscroll_step();
        }
        if changed {
            if let Some(pane) = active {
                self.view(pane).invalidate();
            }
            // The viewer is no feed pane: without this the picture sat in
            // the cache while the page still read "Loading..." until the
            // next unrelated repaint.
            if matches!(self.nav.last(), Some(Nav::Viewer { .. })) {
                self.viewer.invalidate();
            }
        }
        let busy = self.pending > 0
            || self.layouts.pending() > 0
            || self.panes.iter().any(|p| p.staging.is_some())
            || !self.image_inbox.is_empty()
            || active.is_some_and(|p| self.view(p).autoscrolling())
            || taken == REFRESH_PER_TICK;
        let more = drained.cut_short || busy;
        let now = Instant::now();
        {
            let mut perf = self.home.view.perf.borrow_mut();
            perf.tick(started.elapsed());
            if let Some(summary) = perf.due(now) {
                self.platform.log(&format!("SKY:PERF:{}", summary.fields()));
            }
        }
        self.set_interval(ui, drain::interval(0, more));
    }
}

impl LazySkyApp {
    /// Asks for the unread count now and then while signed in (60 s).
    fn poll_unread(&mut self, now: Instant) {
        if self.account.is_none() {
            return;
        }
        let due = self
            .last_unread_poll
            .is_none_or(|at| now.duration_since(at) >= UNREAD_EVERY);
        if !due {
            return;
        }
        self.last_unread_poll = Some(now);
        let seq = self.seq();
        self.ask(seq, Request::UnreadCount { seq }, Route::Unread);
    }
}
