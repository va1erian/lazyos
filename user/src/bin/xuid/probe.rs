//! The UI probe (issue #538): in an image built with `LAZYOS_UI_PROBE=1`
//! (`cfg(lazyos_ui_probe)`), every window's content
//! rectangle is printed on serial whenever the shell is told the window
//! changed, so a session script can click inside a window by its title
//! (`tools/screenshot/README.md`, "Clicking by position or name"):
//!
//! `UI:RECT x=<x> y=<y> w=<w> h=<h> name=window:<title>`
//!
//! Coordinates are physical screen pixels. Apps print their widgets relative
//! to that origin (`UI:WIDGET`). The switch is compile-time (`lazyos_ui_probe`,
//! set by `user/build.rs` from `LAZYOS_UI_PROBE=1`): a normal compositor has
//! no probe code at all, and a probe one never asks the file system (the apps
//! look for `fhs::etc::UI_PROBE`, which the same switch writes). An early
//! version looked the marker up with `stat` from here and sometimes hung the
//! boot right at that call.

use user::trace;

use super::surface::Surface;

/// Say on serial that this compositor prints window rectangles.
pub(super) fn announce() {
    if cfg!(lazyos_ui_probe) {
        trace::line!(trace::Channel::Xuid, "PROBE:ON (UI:RECT lines for windows)");
    }
}

/// Print `surface`'s content rectangle when it is a window and the probe is on.
pub(super) fn window(surface: &Surface) {
    if !cfg!(lazyos_ui_probe) || !surface.is_window() || surface.minimized {
        return;
    }
    let rect = surface.content();
    trace::line!(
        trace::Channel::Ui,
        "RECT x={} y={} w={} h={} name=window:{}",
        rect.x,
        rect.y,
        rect.w,
        rect.h,
        surface.title
    );
    // The title bar too, so a session drags a window by name (issue #161).
    let frame = surface.window();
    trace::line!(
        trace::Channel::Ui,
        "RECT x={} y={} w={} h={} name=title:{}",
        frame.x,
        frame.y,
        frame.w,
        super::theme::title_h(),
        surface.title
    );
}
