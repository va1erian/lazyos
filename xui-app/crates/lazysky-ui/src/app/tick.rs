//! The timer tick: draining the core, the layout thread and the pictures.

use super::*;

impl LazySkyApp {
    /// One timer tick: drain what finished, keep the rows near the view
    /// prepared, report timings.
    pub(super) fn tick(&mut self, ui: &mut Ui<Msg>) {
        let started = Instant::now();
        let drained = drain::drain(&self.events, drain::BUDGET);
        for event in drained.events {
            self.on_event(ui, event);
        }
        // Layout results: a staged page's rows complete it; the rest
        // re-prepare rows already in a feed.
        let mut refreshed: [Vec<crate::layouts::Done>; 2] = [Vec::new(), Vec::new()];
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
        let active = self.active_pane();
        for pane in [HOME, DETAIL] {
            let done = std::mem::take(&mut refreshed[pane as usize]);
            let view = Rc::clone(self.view(pane));
            let mut s = view.state.borrow_mut();
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
                    Ok(bitmap) => cache.insert(key, bitmap),
                    Err(_) => cache.fail(key),
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
