//! The backend event loop: timer firing and one tick's worth of work.

use std::sync::atomic::Ordering;

use xui_core::backend::{Event, TimerId, WidgetId, WindowId};
use xui_core::Rect;

use crate::sys;

use super::{LazyOSBackend, Mode, CLIENT_IDLE_TICKS};

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

    /// Client mode: park until a window's compositor or input-session
    /// endpoint has a message, or the next timer is due (P3.8), instead of a
    /// one-tick receive per window per pass. A window with damage still
    /// waiting (no free buffer until a `FrameDone`) is woken by that event.
    /// More endpoints than one wait can name falls back to a one-tick nap.
    pub(super) fn park_client(&self) {
        let mut handles = [0u64; sys::WAIT_MAX_ENDPOINTS];
        let mut count = 0;
        let mut overflow = false;
        for entry in self.windows.borrow().values() {
            let Some(surface) = entry.client.as_ref() else {
                continue;
            };
            let session = surface.input.map(|session| session.events);
            for handle in core::iter::once(surface.events).chain(session) {
                match handles.get_mut(count) {
                    Some(slot) => {
                        *slot = handle;
                        count += 1;
                    }
                    None => overflow = true,
                }
            }
        }
        let now = sys::clock_ticks();
        let next_timer = self
            .timers
            .borrow()
            .iter()
            .map(|timer| timer.deadline)
            .min();
        let deadline = next_timer
            .unwrap_or(u64::MAX)
            .min(now.saturating_add(if overflow { 1 } else { CLIENT_IDLE_TICKS }))
            .max(now + 1);
        if count == 0 {
            sys::sleep_millis(deadline.saturating_sub(now) * 10);
            return;
        }
        match sys::msg_wait_any(&handles[..count], deadline) {
            Ok(mask) => self.reap_closed(&handles[..count], mask),
            Err(code) if code == -sys::errno::ETIMEDOUT => {}
            // Never spin on a refused wait.
            Err(_) => sys::sleep_millis(10),
        }
    }

    /// A ready endpoint with nothing queued has a closed peer: the
    /// compositor (nothing to draw into: quit) or `inputd` (the window keeps
    /// the compositor's legacy keys). Forget it, or every park would return
    /// at once.
    fn reap_closed(&self, handles: &[u64], mask: u64) {
        for (index, &handle) in handles.iter().enumerate() {
            if mask & (1 << index) == 0 || !matches!(sys::msg_queued(handle), Ok(0)) {
                continue;
            }
            let mut probe = [0u8; 16];
            let closed = matches!(
                sys::msg_recv(handle, &mut probe, sys::EXPIRED_DEADLINE),
                Err(code) if code == -sys::errno::EPIPE
            );
            if !closed {
                continue;
            }
            for entry in self.windows.borrow_mut().values_mut() {
                let Some(surface) = entry.client.as_mut() else {
                    continue;
                };
                if surface.events == handle {
                    self.quit.store(true, Ordering::Relaxed);
                } else if surface
                    .input
                    .is_some_and(|session| session.events == handle)
                {
                    surface.input = None;
                }
            }
        }
    }

    /// Hand the app the `Configure` that `ClientWindow::open` consumed to
    /// size the first buffer, so it re-flows exactly as if it had arrived
    /// after startup. The buffer and painting surface were already created at
    /// that size, so only the `Resize` is delivered: attaching another buffer
    /// could fail and lose the event for nothing.
    fn replay_open_configure(&self, window: WindowId) {
        let (pending, events) = {
            let mut windows = self.windows.borrow_mut();
            let Some(surface) = windows
                .get_mut(&window.raw())
                .and_then(|entry| entry.client.as_mut())
            else {
                return;
            };
            (
                surface.pending_configure.take(),
                std::mem::take(&mut surface.pending_events),
            )
        };
        if let Some((width, height)) = pending {
            self.deliver(window, WidgetId::NONE, &Event::Resize { width, height });
            self.add_damage(window, Rect::new(0, 0, width, height));
            self.dirty.store(true, Ordering::Relaxed);
        }
        // Input that raced the open, after the resize it was typed against.
        for event in events {
            self.route_client_event(window, event);
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
