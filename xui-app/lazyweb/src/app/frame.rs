//! Showing what the engine painted: the frame (or its damage) goes into the page view, the frame
//! clock is fed, and the controls are announced when the page's size changed.

use std::time::Instant;

use xui_core::app::Ui;
use xui_core::widget::Placeable;

use super::{Browser, Msg};

impl Browser {
    pub(super) fn show_frame(&mut self, ui: &Ui<Msg>) {
        self.page.add_bands(xui_webkit::bands::take());
        if let Some(update) = self.mailbox.take_update(std::mem::take(&mut self.spare)) {
            if self.state.crashed {
                // A picture painted just before the page's process went away.
                self.spare = update.full.map(|frame| frame.rgba).unwrap_or_default();
                return;
            }
            let full = update.full.is_some();
            // Only the page's answer to the wheel, no pixels: nothing was drawn for the frame clock to count.
            let pixels = full || !update.deltas.is_empty();
            let before = self.page.picture_size();
            let started = Instant::now();
            let applied = self.page.apply(update);
            // A whole picture of a new size: the page was laid out at the view's size (what a
            // session waits for after it resized the window).
            match self.page.picture_size() {
                Some((w, h)) if full && Some((w, h)) != before => {
                    self.trace(&format!("INFO:RESIZE:{w}x{h}"));
                }
                _ => {}
            }
            let took = started.elapsed();
            let recorded = pixels.then(|| {
                self.clock.record(
                    started + took,
                    took,
                    full,
                    self.page.picture_parts(),
                    self.page.take_draw_time(),
                )
            });
            if let Some(line) = recorded.flatten() {
                crate::marker(&line);
            }
            if let Some(line) = self.page.spec_line() {
                crate::marker(&line);
            }
            if let Some(line) = self.page.smooth_line() {
                crate::marker(&line);
            }
            if applied.resync {
                self.mailbox.request_full();
            }
            if !applied.spare.is_empty() {
                self.spare = applied.spare;
            }
            self.frames += u64::from(pixels);
            // The first frame, and the first after the window changed size: where the controls are.
            let page = ui.bounds(self.page.id());
            if self.announced.get() != (page.width(), page.height()) {
                self.announce(ui);
            }
        }
    }
}
