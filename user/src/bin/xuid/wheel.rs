//! Mouse wheel routing: the wheel goes to the window under the pointer, not
//! the focused one, and only when the pointer is over the window's content.

use user::messenger::display::wire;

use super::compositor::Compositor;
use super::surface::Surface;
use super::window::{contains, forward, relative};

/// The id of the window that receives a wheel roll at `point`: the topmost
/// visible window under it, provided the point is in that window's content.
/// A point on a title bar or border, or over no window, yields `None` (the
/// wheel never falls through to a window below the one it is over).
pub(super) fn wheel_target(surfaces: &[Surface], point: (i32, i32)) -> Option<u64> {
    surfaces
        .iter()
        .rev()
        .find(|surface| !surface.desktop && !surface.minimized && contains(surface.window(), point))
        .filter(|surface| contains(surface.content(), point))
        .map(|surface| surface.id)
}

impl Compositor {
    /// The wheel rolled `delta` notches (positive scrolls up). An open menu or
    /// a drag (window or drag & drop) owns the pointer, so the wheel is ignored.
    pub(super) fn pointer_wheel(&mut self, delta: i32) {
        if delta == 0
            || self.drag.is_some()
            || self.drag_session.is_some()
            || super::menu::is_open()
        {
            return;
        }
        let Some(id) = wheel_target(&self.surfaces, self.pointer) else {
            return;
        };
        let (x, y) = relative(&self.surfaces, id, self.pointer);
        let body = wire::encode_pointer_wheel_args(&wire::PointerWheelArgs { x, y, delta });
        forward(
            &self.surfaces,
            &mut self.scratch,
            Some(id),
            wire::METHOD_POINTERWHEEL,
            body,
        );
    }
}

/// Boot check of the wheel routing rules: `XUID:WHEEL:PASS` or
/// `XUID:WHEEL:FAIL`.
pub(super) fn selftest_wheel_routing() -> &'static str {
    use super::window::test_surface;

    // Two overlapping windows: `1` below `2`. A window at (x, y) with a 10x10
    // content is `10 + 2*BORDER` wide; probe its content and its title bar.
    let mut below = test_surface(1, false, false);
    below.x = 0;
    below.y = 0;
    let mut above = test_surface(2, false, false);
    above.x = 5;
    above.y = 5;
    let stack = alloc::vec![below, above];
    let above_content = stack[1].content();
    let above_title = stack[1].title_bar();
    let inside = |rect: user::messenger::display::Rect| (rect.x + 1, rect.y + 1);

    // Over the top window's content: the top window, even where it overlaps.
    let top = wheel_target(&stack, inside(above_content)) == Some(2);
    // Over the top window's title bar: nobody (no fall-through to `1`).
    let title = wheel_target(&stack, inside(above_title)).is_none();
    // Over the lower window only: the lower window.
    let lower_content = stack[0].content();
    let lower = wheel_target(&stack, inside(lower_content)) == Some(1);
    // Over nothing.
    let nothing = wheel_target(&stack, (5000, 5000)).is_none();

    // A minimized window and a desktop never receive it.
    let mut hidden = test_surface(3, true, false);
    hidden.x = 100;
    let mut desktop = test_surface(4, false, true);
    desktop.x = 100;
    let probe = inside(hidden.content());
    let skipped = wheel_target(&[hidden, desktop], probe).is_none();

    if top && title && lower && nothing && skipped {
        "XUID:WHEEL:PASS\n"
    } else {
        "XUID:WHEEL:FAIL\n"
    }
}
