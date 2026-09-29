//! The backend event loop: timer firing and one tick's worth of work.

use std::sync::atomic::Ordering;

use xui_core::backend::{Event, TimerId, WidgetId, WindowId};

use crate::sys;

use super::{LazyOSBackend, Mode};

impl LazyOSBackend {
    /// Deliver every due timer's `Timer` event and re-arm it for its period.
    fn fire_timers(&self, window: WindowId) {
        let now = sys::clock_ticks();
        let mut due = Vec::new();
        for timer in self.timers.borrow_mut().iter_mut() {
            if now >= timer.deadline {
                due.push(timer.id);
                timer.deadline = now.saturating_add(timer.millis.div_ceil(10));
            }
        }
        for id in due {
            self.deliver(window, WidgetId::NONE, &Event::Timer { id: TimerId(id) });
        }
    }

    /// One event-loop iteration: drain input, flush widget messages, run due
    /// timers, and repaint when something is dirty.
    pub(super) fn tick(&self, window: WindowId) {
        match &self.mode {
            Mode::Owner { .. } => self.pump_input(window),
            Mode::Client(state) => {
                let events = state.borrow().events;
                self.pump_client_input(window, events);
            }
        }
        // A wake drains the message queue; widget mappers enqueue while an
        // input record is being routed, so this runs after every batch.
        self.deliver(window, WidgetId::NONE, &Event::Wake);
        self.fire_timers(window);
        // Timer messages joined the queue after the wake above; drain them in
        // the same pass so a refresh paints without a poll-period delay.
        self.deliver(window, WidgetId::NONE, &Event::Wake);
        if self.dirty.swap(false, Ordering::Relaxed) {
            self.present(window);
        }
    }
}
