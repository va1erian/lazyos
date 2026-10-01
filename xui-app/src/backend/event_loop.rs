//! The backend event loop: timer firing and one tick's worth of work.

use std::sync::atomic::Ordering;

use xui_core::backend::{Event, TimerId, WidgetId, WindowId};

use crate::sys;

use super::{LazyOSBackend, Mode};

impl LazyOSBackend {
    /// Deliver every due timer's `Timer` event and re-arm it for its period.
    ///
    /// Only `window`'s own timers fire: the backend serves several windows and
    /// a `Timer` event must reach the window that armed it.
    fn fire_timers(&self, window: WindowId) {
        let now = sys::clock_ticks();
        let mut due = Vec::new();
        for timer in self.timers.borrow_mut().iter_mut() {
            if timer.window == window.raw() && now >= timer.deadline {
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
            Mode::Client(_) => {
                let (events, session) = self
                    .windows
                    .borrow()
                    .get(&window.raw())
                    .and_then(|entry| entry.client.as_ref())
                    .map(|surface| (surface.events, surface.input.map(|s| s.events)))
                    .unzip();
                if let Some(events) = events {
                    self.replay_open_configure(window);
                    self.pump_client_input(window, events);
                }
                if let Some(Some(session)) = session {
                    self.pump_session_input(window, session);
                }
            }
        }
        // A wake drains the message queue; widget mappers enqueue while an
        // input record is being routed, so this runs after every batch.
        self.deliver(window, WidgetId::NONE, &Event::Wake);
        self.fire_timers(window);
        // Timer messages joined the queue after the wake above; drain them in
        // the same pass so a refresh paints without a poll-period delay.
        self.deliver(window, WidgetId::NONE, &Event::Wake);
        if self.needs_present(window) {
            self.present(window);
        }
    }

    /// Hand the app the `Configure` that `ClientWindow::open` consumed to
    /// size the first buffer, so it re-flows exactly as if it had arrived
    /// after startup.
    fn replay_open_configure(&self, window: WindowId) {
        let pending = self
            .windows
            .borrow_mut()
            .get_mut(&window.raw())
            .and_then(|entry| entry.client.as_mut())
            .and_then(|surface| surface.pending_configure.take());
        if let Some((width, height)) = pending {
            self.apply_configure(window, width, height);
        }
    }

    /// Whether `window` has something to commit. Owner mode has one screen
    /// and one flag. A client has a surface per window, and the flag is
    /// shared, so one window's tick must not consume another's pending
    /// repaint (a folder window opened, or changed by input, while a sibling
    /// ticks): the per-window damage map decides instead.
    fn needs_present(&self, window: WindowId) -> bool {
        let flagged = self.dirty.swap(false, Ordering::Relaxed);
        match &self.mode {
            Mode::Owner { .. } => flagged,
            Mode::Client(_) => self.damage.borrow().contains_key(&window.raw()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_window::ClientState;
    use crate::display::Client;
    use crate::sys::DisplayInfo;
    use xui_core::Rect;

    fn client_backend() -> LazyOSBackend {
        LazyOSBackend::with_mode(Mode::Client(std::cell::RefCell::new(ClientState::new(
            Client::detached(),
        ))))
    }

    fn window(raw: u64) -> WindowId {
        WindowId::from_raw(raw)
    }

    #[test]
    fn one_windows_tick_does_not_consume_another_windows_repaint() {
        let backend = client_backend();
        // Window B's widgets are created (damage and the shared flag set)
        // while window A ticks; A's tick then clears the shared flag.
        backend.add_damage(window(2), Rect::new(0, 0, 10, 10));
        backend.dirty.store(true, Ordering::Relaxed);
        assert!(!backend.needs_present(window(1)), "A had no damage");
        assert!(backend.needs_present(window(2)), "B's repaint was lost");
    }

    #[test]
    fn a_window_stays_pending_until_it_commits() {
        let backend = client_backend();
        backend.add_damage(window(2), Rect::new(0, 0, 4, 4));
        // Cross-window change: A ticks (twice) before B does.
        assert!(!backend.needs_present(window(1)));
        assert!(!backend.needs_present(window(1)));
        assert!(backend.needs_present(window(2)));
        assert!(
            backend.needs_present(window(2)),
            "damage persists until taken"
        );
        assert_eq!(
            backend.take_damage(window(2), Rect::new(0, 0, 8, 8)),
            Rect::new(0, 0, 4, 4)
        );
        assert!(!backend.needs_present(window(2)));
    }

    #[test]
    fn owner_mode_still_follows_the_shared_flag() {
        let backend = LazyOSBackend::with_mode(Mode::Owner {
            display: DisplayInfo::default(),
        });
        assert!(!backend.needs_present(window(1)));
        backend.dirty.store(true, Ordering::Relaxed);
        assert!(backend.needs_present(window(1)));
        assert!(!backend.needs_present(window(1)), "the flag is consumed");
    }
}
