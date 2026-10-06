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

use std::io::Write;
use std::sync::OnceLock;

/// Whether this image carries the probe marker (looked up once).
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::path::Path::new(fhs::etc::UI_PROBE).exists())
}

/// The `UI:RECT` line for a screen rectangle.
pub fn rect_line(name: &str, x: i32, y: i32, w: i32, h: i32) -> String {
    format!("UI:RECT x={x} y={y} w={w} h={h} name={name}")
}

/// The `UI:WIDGET` line for a rectangle inside the window titled `window`.
pub fn widget_line(window: &str, name: &str, x: i32, y: i32, w: i32, h: i32) -> String {
    format!("UI:WIDGET x={x} y={y} w={w} h={h} name={name} window={window}")
}

/// Write `line` to the console device (serial), else to stdout, as one
/// write: `writeln!` sends the text and the newline separately, and another
/// task's marker landing between the two glued itself onto the line, so the
/// session could not parse the name (seen in `tray.json`).
fn emit(line: &str) {
    let whole = format!("{line}\n");
    let written = std::fs::OpenOptions::new()
        .write(true)
        .open(fhs::dev::CONSOLE)
        .and_then(|mut device| device.write_all(whole.as_bytes()));
    if written.is_err() {
        let _ = std::io::stdout().lock().write_all(whole.as_bytes());
    }
}

/// Print a screen rectangle when the probe is on.
pub fn rect(name: &str, x: i32, y: i32, w: i32, h: i32) {
    if enabled() {
        emit(&rect_line(name, x, y, w, h));
    }
}

/// Print a window-relative rectangle when the probe is on.
pub fn widget(window: &str, name: &str, x: i32, y: i32, w: i32, h: i32) {
    if enabled() {
        emit(&widget_line(window, name, x, y, w, h));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_put_the_free_text_last() {
        assert_eq!(
            rect_line("menu:Text Editor", 28, 500, 212, 24),
            "UI:RECT x=28 y=500 w=212 h=24 name=menu:Text Editor"
        );
        assert_eq!(
            widget_line("MOD Player", "play_button", 68, 142, 52, 28),
            "UI:WIDGET x=68 y=142 w=52 h=28 name=play_button window=MOD Player"
        );
    }
}
