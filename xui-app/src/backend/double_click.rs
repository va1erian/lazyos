//! Double-click synthesis for the LazyOS backend.
//!
//! Neither the kernel nor `xuid` reports a double-click: every press arrives as
//! a plain [`Event::MouseDown`]. Widgets such as `xui-core`'s `IconView` emit
//! their activation (`Msg::Activate`) only for [`Event::MouseDoubleClick`], so
//! the backend recognizes the second press of a pair itself.
//!
//! This is the pure part, modelled on `xui-canvas`'s
//! `app::double_click::ClickTracker`. That tracker measures with `std::time::
//! Instant`; LazyOS has no monotonic `Instant` here, so this one takes the PIT
//! tick count ([`sys::clock_ticks`](crate::sys::clock_ticks), 100 Hz) as an
//! explicit argument. The two presses must share the window, the target widget
//! and the button, and land within the interval and the per-axis distance. A
//! pair consumes the sequence, so a third quick press is a fresh first press.
//! Modifiers are deliberately not part of the pairing, matching Win32.

use xui_core::backend::{Event, WidgetId, WindowId};
use xui_core::{Modifiers, MouseButton};

/// Default double-click interval in PIT ticks (100 Hz): 500 ms.
pub(super) const DOUBLE_CLICK_TICKS: u64 = 50;
/// Default largest gap, in pixels on either axis, between the two presses.
pub(super) const DOUBLE_CLICK_DISTANCE: i32 = 4;

/// What a press turned out to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PressKind {
    /// A plain press: the caller delivers [`Event::MouseDown`].
    Down,
    /// The second press of a double-click: the caller delivers
    /// [`Event::MouseDoubleClick`] in place of the second `MouseDown`, matching
    /// Win32, where `WM_*BUTTONDBLCLK` replaces the second `WM_*BUTTONDOWN`.
    DoubleClick,
}

impl PressKind {
    /// The event this press delivers, at `(x, y)` in the widget's client
    /// coordinates.
    pub(super) fn event(self, x: i32, y: i32, button: MouseButton, modifiers: Modifiers) -> Event {
        match self {
            PressKind::Down => Event::MouseDown {
                x,
                y,
                button,
                modifiers,
            },
            PressKind::DoubleClick => Event::MouseDoubleClick {
                x,
                y,
                button,
                modifiers,
            },
        }
    }
}

/// One remembered press, in window (not node-local) coordinates.
///
/// The window and target are part of the identity: a backend serves several
/// windows at once, so a press only pairs with one on the same window and
/// widget.
#[derive(Clone, Copy, Debug)]
struct Press {
    at: u64,
    x: i32,
    y: i32,
    button: MouseButton,
    window: WindowId,
    target: WidgetId,
}

/// Recognizes the second press of a double-click from the sequence of presses
/// a backend routes.
pub(super) struct ClickTracker {
    interval: u64,
    /// How far the second press may land from the first, on x and on y.
    distance: (i32, i32),
    last: Option<Press>,
}

impl ClickTracker {
    /// A tracker using the documented fallback interval and distance (there is
    /// no system setting to query on LazyOS).
    pub(super) const fn new() -> ClickTracker {
        ClickTracker::with_extent(
            DOUBLE_CLICK_TICKS,
            DOUBLE_CLICK_DISTANCE,
            DOUBLE_CLICK_DISTANCE,
        )
    }

    /// [`ClickTracker::new`] with the distance in design pixels at `scale`.
    pub(super) const fn scaled(scale: i32) -> ClickTracker {
        ClickTracker::with_extent(
            DOUBLE_CLICK_TICKS,
            DOUBLE_CLICK_DISTANCE * scale,
            DOUBLE_CLICK_DISTANCE * scale,
        )
    }

    /// A tracker that pairs presses no further apart than `interval` ticks and
    /// no more than `dx`/`dy` pixels from each other on either axis. A negative
    /// extent is treated as zero.
    pub(super) const fn with_extent(interval: u64, dx: i32, dy: i32) -> ClickTracker {
        ClickTracker {
            interval,
            distance: (if dx < 0 { 0 } else { dx }, if dy < 0 { 0 } else { dy }),
            last: None,
        }
    }

    /// Classifies a press of `button` on `target` at `(x, y)` in window
    /// coordinates at tick `now`.
    ///
    /// A press pairs with the one before it when it repeats its button, window
    /// and widget within the interval and the distance box. The pair consumes
    /// the sequence (the stored press is cleared), so the next quick press
    /// starts over.
    pub(super) fn press(
        &mut self,
        now: u64,
        window: WindowId,
        x: i32,
        y: i32,
        button: MouseButton,
        target: WidgetId,
    ) -> PressKind {
        let press = Press {
            at: now,
            x,
            y,
            button,
            window,
            target,
        };
        let repeats = self.last.is_some_and(|last| self.matches(&last, &press));
        self.last = if repeats { None } else { Some(press) };
        if repeats {
            PressKind::DoubleClick
        } else {
            PressKind::Down
        }
    }

    /// Forget a pending first press that targeted `target`, because the widget
    /// was removed: a later press must not complete its pair.
    pub(super) fn forget_widget(&mut self, target: WidgetId) {
        if self.last.is_some_and(|last| last.target == target) {
            self.last = None;
        }
    }

    /// Forget the pending first press unless its target satisfies `exists`
    /// (the widget, or an ancestor removal cascade, took it away).
    pub(super) fn forget_unless(&mut self, exists: impl Fn(WidgetId) -> bool) {
        if self.last.is_some_and(|last| !exists(last.target)) {
            self.last = None;
        }
    }

    /// Forget a pending first press in `window`, because it was closed.
    pub(super) fn forget_window(&mut self, window: WindowId) {
        if self.last.is_some_and(|last| last.window == window) {
            self.last = None;
        }
    }

    /// Whether a first press is pending. Used by the backend's reset tests.
    #[cfg(test)]
    pub(super) fn has_pending(&self) -> bool {
        self.last.is_some()
    }

    /// Whether `press` repeats `last`.
    fn matches(&self, last: &Press, press: &Press) -> bool {
        last.window == press.window
            && last.button == press.button
            && last.target == press.target
            && press.at.saturating_sub(last.at) <= self.interval
            && within(press.x, last.x, self.distance.0)
            && within(press.y, last.y, self.distance.1)
    }
}

/// Whether `a` and `b` are no further apart than `distance`, using `i64` so a
/// coordinate pair straddling `i32::MIN`/`i32::MAX` cannot overflow.
fn within(a: i32, b: i32, distance: i32) -> bool {
    (i64::from(a) - i64::from(b)).abs() <= i64::from(distance)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: WindowId = WindowId::from_raw(1);
    const OTHER_WINDOW: WindowId = WindowId::from_raw(2);
    const WIDGET: WidgetId = WidgetId::from_raw(7);
    const OTHER: WidgetId = WidgetId::from_raw(8);

    fn tracker() -> ClickTracker {
        ClickTracker::new()
    }

    #[test]
    fn two_presses_within_the_interval_and_distance_double_click() {
        let mut tracker = tracker();
        assert_eq!(
            tracker.press(0, WINDOW, 10, 10, MouseButton::Left, WIDGET),
            PressKind::Down
        );
        assert_eq!(
            tracker.press(10, WINDOW, 12, 13, MouseButton::Left, WIDGET),
            PressKind::DoubleClick
        );
    }

    #[test]
    fn the_interval_and_distance_bounds_are_inclusive() {
        let mut tracker = tracker();
        tracker.press(0, WINDOW, 0, 0, MouseButton::Left, WIDGET);
        assert_eq!(
            tracker.press(
                DOUBLE_CLICK_TICKS,
                WINDOW,
                DOUBLE_CLICK_DISTANCE,
                -DOUBLE_CLICK_DISTANCE,
                MouseButton::Left,
                WIDGET
            ),
            PressKind::DoubleClick
        );
    }

    #[test]
    fn a_press_one_tick_past_the_interval_is_a_new_first_press() {
        let mut tracker = tracker();
        tracker.press(0, WINDOW, 10, 10, MouseButton::Left, WIDGET);
        assert_eq!(
            tracker.press(
                DOUBLE_CLICK_TICKS + 1,
                WINDOW,
                10,
                10,
                MouseButton::Left,
                WIDGET
            ),
            PressKind::Down
        );
    }

    #[test]
    fn a_press_one_pixel_past_the_distance_is_a_new_first_press() {
        let mut tracker = tracker();
        tracker.press(0, WINDOW, 10, 10, MouseButton::Left, WIDGET);
        assert_eq!(
            tracker.press(
                5,
                WINDOW,
                10 + DOUBLE_CLICK_DISTANCE + 1,
                10,
                MouseButton::Left,
                WIDGET
            ),
            PressKind::Down
        );
        assert_eq!(
            tracker.press(
                10,
                WINDOW,
                10,
                10 + DOUBLE_CLICK_DISTANCE + 1,
                MouseButton::Left,
                WIDGET
            ),
            PressKind::Down
        );
    }

    #[test]
    fn a_different_button_is_not_a_double_click() {
        let mut tracker = tracker();
        tracker.press(0, WINDOW, 10, 10, MouseButton::Left, WIDGET);
        assert_eq!(
            tracker.press(0, WINDOW, 10, 10, MouseButton::Right, WIDGET),
            PressKind::Down
        );
    }

    #[test]
    fn a_different_widget_is_not_a_double_click() {
        let mut tracker = tracker();
        tracker.press(0, WINDOW, 10, 10, MouseButton::Left, WIDGET);
        assert_eq!(
            tracker.press(0, WINDOW, 10, 10, MouseButton::Left, OTHER),
            PressKind::Down
        );
    }

    #[test]
    fn a_different_window_is_not_a_double_click() {
        let mut tracker = tracker();
        tracker.press(0, WINDOW, 10, 10, MouseButton::Left, WIDGET);
        assert_eq!(
            tracker.press(0, OTHER_WINDOW, 10, 10, MouseButton::Left, WIDGET),
            PressKind::Down
        );
    }

    #[test]
    fn a_third_quick_press_starts_a_new_sequence() {
        let mut tracker = tracker();
        assert_eq!(
            tracker.press(0, WINDOW, 10, 10, MouseButton::Left, WIDGET),
            PressKind::Down
        );
        assert_eq!(
            tracker.press(10, WINDOW, 10, 10, MouseButton::Left, WIDGET),
            PressKind::DoubleClick
        );
        // The pair consumed the sequence, so the third quick press is a fresh
        // first press, and only a fourth would make another double-click.
        assert_eq!(
            tracker.press(20, WINDOW, 10, 10, MouseButton::Left, WIDGET),
            PressKind::Down
        );
        assert_eq!(
            tracker.press(30, WINDOW, 10, 10, MouseButton::Left, WIDGET),
            PressKind::DoubleClick
        );
    }

    #[test]
    fn a_clock_that_goes_backwards_saturates_to_no_elapsed_time() {
        // A wrapped or reset tick counter must not underflow. `now` below
        // `last.at` saturates to zero elapsed ticks, so the pair still
        // completes (mirroring `Instant::saturating_duration_since`). A `u64`
        // tick cannot wrap within any realistic uptime; this only guards the
        // arithmetic.
        let mut tracker = tracker();
        tracker.press(1_000, WINDOW, 0, 0, MouseButton::Left, WIDGET);
        assert_eq!(
            tracker.press(0, WINDOW, 0, 0, MouseButton::Left, WIDGET),
            PressKind::DoubleClick
        );
    }

    #[test]
    fn a_negative_distance_is_treated_as_zero() {
        let mut tracker = ClickTracker::with_extent(DOUBLE_CLICK_TICKS, -1, 4);
        tracker.press(0, WINDOW, 1, 1, MouseButton::Left, WIDGET);
        // The x extent clamped to zero, so one pixel across is too far.
        assert_eq!(
            tracker.press(0, WINDOW, 2, 1, MouseButton::Left, WIDGET),
            PressKind::Down
        );
        // The y extent is untouched (the refused press became the first one).
        assert_eq!(
            tracker.press(0, WINDOW, 2, 5, MouseButton::Left, WIDGET),
            PressKind::DoubleClick
        );
    }

    #[test]
    fn forgetting_a_removed_widget_drops_only_its_press() {
        let mut tracker = tracker();
        tracker.press(0, WINDOW, 1, 1, MouseButton::Left, WIDGET);
        tracker.forget_widget(OTHER);
        assert!(tracker.has_pending(), "another widget's press is kept");
        tracker.forget_widget(WIDGET);
        assert!(!tracker.has_pending());
    }

    #[test]
    fn forgetting_a_closed_window_drops_only_its_press() {
        let mut tracker = tracker();
        tracker.press(0, OTHER_WINDOW, 1, 1, MouseButton::Left, WIDGET);
        tracker.forget_window(WINDOW);
        assert!(tracker.has_pending(), "another window's press is kept");
        tracker.forget_window(OTHER_WINDOW);
        assert!(!tracker.has_pending());
    }
}
