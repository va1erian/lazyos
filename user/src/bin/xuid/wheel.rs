//! Mouse wheel routing: the wheel goes to what is under the pointer, not the
//! focused window: a shell panel, else the topmost window when the pointer is
//! over its content, else the desktop when no window covers the point.

use user::messenger::display::wire;

use super::compositor::Compositor;
use super::surface::Surface;
use super::window::{contains, forward, relative};

/// The id of the surface that receives a wheel roll at `point`. A point on a
/// window's title bar or border yields `None`: the wheel never falls through
/// to whatever lies below the window it is over.
pub(super) fn wheel_target(surfaces: &[Surface], point: (i32, i32)) -> Option<u64> {
    let under = |keep: fn(&Surface) -> bool| {
        surfaces
            .iter()
            .rev()
            .find(move |surface| keep(surface) && contains(surface.window(), point))
    };
    if let Some(panel) = under(Surface::is_panel) {
        return Some(panel.id);
    }
    match under(|surface| surface.is_window() && !surface.minimized) {
        Some(window) => contains(window.content(), point).then_some(window.id),
        None => under(Surface::is_desktop).map(|desktop| desktop.id),
    }
}

impl Compositor {
    /// The wheel rolled `delta` notches (positive scrolls up). A drag (window
    /// or drag & drop) owns the pointer, so the wheel is ignored; a desktop or
    /// panel holding the pointer grab gets it.
    pub(super) fn pointer_wheel(&mut self, delta: i32) {
        if delta == 0 || self.drag.is_some() || self.drag_session.is_some() {
            return;
        }
        let grabbed = self.grab.map(|(id, _)| id);
        let Some(id) = grabbed.or_else(|| wheel_target(&self.surfaces, self.pointer)) else {
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
    use user::messenger::display::Rect;

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
    let inside = |rect: Rect| (rect.x + 1, rect.y + 1);

    // Over the top window's content: the top window, even where it overlaps.
    let top = wheel_target(&stack, inside(above_content)) == Some(2);
    // Over the top window's title bar: nobody (no fall-through to `1`).
    let title = wheel_target(&stack, inside(above_title)).is_none();
    // Over the lower window only: the lower window.
    let lower_content = stack[0].content();
    let lower = wheel_target(&stack, inside(lower_content)) == Some(1);
    // Over nothing.
    let nothing = wheel_target(&stack, (5000, 5000)).is_none();

    // A minimized window never receives it; the desktop under it does.
    let mut hidden = test_surface(3, true, false);
    hidden.x = 100;
    let mut desktop = test_surface(4, false, true);
    (desktop.w, desktop.h) = (800, 600);
    let probe = inside(hidden.content());
    let layers = alloc::vec![desktop, hidden];
    let to_desktop = wheel_target(&layers, probe) == Some(4);

    // A panel over a window's content takes it.
    let mut panel = test_surface(5, false, false);
    panel.role = wire::ROLE_PANEL;
    (panel.w, panel.h) = (50, 50);
    let mut covered = stack;
    covered.insert(0, panel);
    let to_panel = wheel_target(&covered, inside(above_content)) == Some(5);

    if top && title && lower && nothing && to_desktop && to_panel {
        "XUID:WHEEL:PASS\n"
    } else {
        "XUID:WHEEL:FAIL\n"
    }
}
