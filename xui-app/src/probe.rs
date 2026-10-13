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

/// The text after `UI:` of a screen rectangle: `RECT x= y= w= h= name=<name>`.
/// The name is free text and stays last and unquoted, which is what the
/// session scripts parse.
fn rect_text(name: &str, x: i32, y: i32, w: i32, h: i32) -> String {
    format!("RECT x={x} y={y} w={w} h={h} name={name}")
}

/// The text after `UI:` of a rectangle inside the window titled `window`.
fn widget_text(window: &str, name: &str, x: i32, y: i32, w: i32, h: i32) -> String {
    format!("WIDGET x={x} y={y} w={w} h={h} name={name} window={window}")
}

/// Print a screen rectangle when the probe is on (`UI:RECT ...`).
pub fn rect(name: &str, x: i32, y: i32, w: i32, h: i32) {
    if enabled() {
        trace::line!(Channel::Ui, "{}", rect_text(name, x, y, w, h));
    }
}

/// Print a window-relative rectangle when the probe is on (`UI:WIDGET ...`).
pub fn widget(window: &str, name: &str, x: i32, y: i32, w: i32, h: i32) {
    if enabled() {
        trace::line!(Channel::Ui, "{}", widget_text(window, name, x, y, w, h));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole line the sink writes for `text` on `Channel::Ui`.
    fn on_the_wire(text: &str) -> String {
        let mut line = tracefmt::LineBuf::<{ tracefmt::LINE_MAX }>::new();
        let _ = tracefmt::write::line(&mut line, Channel::Ui, format_args!("{text}"));
        String::from_utf8(line.finish().to_vec()).unwrap()
    }

    #[test]
    fn lines_put_the_free_text_last() {
        assert_eq!(
            on_the_wire(&rect_text("menu:Text Editor", 28, 500, 212, 24)),
            "UI:RECT x=28 y=500 w=212 h=24 name=menu:Text Editor\n"
        );
        assert_eq!(
            on_the_wire(&widget_text("MOD Player", "play_button", 68, 142, 52, 28)),
            "UI:WIDGET x=68 y=142 w=52 h=28 name=play_button window=MOD Player\n"
        );
    }
}
