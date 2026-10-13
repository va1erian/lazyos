//! The UI probe (issue #538): named on-screen rectangles for session scripts.
//!
//! In an image built with `LAZYOS_UI_PROBE=1` (the marker file
//! `fhs::etc::UI_PROBE` exists), apps print where their named controls are,
//! and `tools/screenshot/qemu_session.py` clicks them by name instead of by
//! hand-measured relative mouse moves (`tools/screenshot/README.md`,
//! "Clicking by position or name"). Two serial lines, physical pixels:
//!
//! * `UI:RECT x= y= w= h= name=<name>`: a screen rectangle (the shell's
//!   start button and menu rows; `xuid` prints `name=window:<title>` for
//!   each window's content);
//! * `UI:WIDGET x= y= w= h= name=<widget> window=<title>`: a rectangle
//!   relative to the content of the window titled `<title>`.
//!
//! The newest line for a name wins. A normal image has no marker: one failed
//! lookup, then nothing. A line goes to the kernel console (serial included)
//! even when the app's stdout is a Terminal window's pty, as the LazyRAD
//! player's markers do.

use std::sync::OnceLock;

use crate::trace::{self, Channel};

/// Whether this image carries the probe marker and the `ui` channel prints
/// (looked up once). The marker file is what tells a session image from a
/// normal one: `LAZYOS_UI_PROBE=1` writes it and adds `ui` to the trace mask.
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        trace::enabled(Channel::Ui) && std::path::Path::new(fhs::etc::UI_PROBE).exists()
    })
}

/// Print a screen rectangle when the probe is on: `UI:RECT x= y= w= h=
/// name=<name>`. The name is free text and stays last and unquoted, which is
/// what the session scripts parse.
pub fn rect(name: &str, x: i32, y: i32, w: i32, h: i32) {
    if enabled() {
        trace::line!(Channel::Ui, "RECT x={x} y={y} w={w} h={h} name={name}");
    }
}

/// Print a window-relative rectangle when the probe is on: `UI:WIDGET x= y=
/// w= h= name=<name> window=<window>`.
pub fn widget(window: &str, name: &str, x: i32, y: i32, w: i32, h: i32) {
    if enabled() {
        trace::line!(
            Channel::Ui,
            "WIDGET x={x} y={y} w={w} h={h} name={name} window={window}"
        );
    }
}
