//! LazyWebPlus: LazyWeb's chrome on WebKit's engine (docs/lazywebplus.md).
//!
//! The window is LazyWeb's: a menu bar, a toolbar with a throbber, a status bar with a padlock
//! and the running downloads, Ctrl+L and the other keys, a browsing history and downloads folder
//! in the user's home, pages of its own (`about:history`, `about:downloads`, `about:lazyweb`,
//! the start page) and `mailto:` and other schemes handed to the app registered for them. The
//! page is WebKit's: its UI process (`webkit/port/app/`, C++) paints it into a bitmap and the
//! page view ([`page`], from `xui-webkit`) shows it and forwards the pointer and keys.
//!
//! The parts with no window are tested on the host:
//!
//! - [`address`]: what the address bar turns typed text into.
//! - [`policy`]: which navigations the engine may make, which go to another app, which are
//!   the browser's own commands, and what a page may do without a user gesture.
//! - [`visits`]: the browsing history kept per user.
//! - [`downloads`]: naming and reserving downloads in the Downloads folder.
//! - [`pages`]: the built-in `about:` pages.
//! - [`internal`]: those pages as the `data:` URLs the engine shows.
//!
//! The window itself ([`app`], [`chrome`]) is driven offscreen against a recording engine in
//! `tests/`. On LazyOS, `ffi` is the C ABI the UI process calls.

pub mod address;
pub mod app;
pub mod chrome;
pub mod context;
pub mod dialogs;
pub mod downloads;
pub mod handoff;
pub mod indicators;
pub mod internal;
pub mod keys;
pub mod pages;
pub mod policy;
pub mod transfers;
pub mod visits;

#[cfg(all(target_os = "linux", target_env = "musl"))]
mod ffi;

/// Whether the markers that name a page, an address or a file (the ones a person's privacy cares about)
/// may be printed: only in an image built with the UI probe (`LAZYOS_UI_PROBE=1`), as `engine.h` promises.
static NAMED_MARKERS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Allows or forbids the markers that name pages (the window sets it from the UI probe at start).
pub fn allow_named_markers(on: bool) {
    NAMED_MARKERS.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Prints `WEBPLUS:<line>` when named markers are allowed.
pub fn named_marker(line: &str) {
    if NAMED_MARKERS.load(std::sync::atomic::Ordering::Relaxed) {
        println!("WEBPLUS:{line}");
    }
}

/// `text` fit for one serial marker line: control characters (a title can
/// hold newlines) become spaces.
pub fn marker_text(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The app's id: its data folder is `$HOME/.apps/<SYSTEM_NAME>`.
pub const SYSTEM_NAME: &str = "os.lazy.lazywebplus";
/// The history file in that folder.
pub const HISTORY_FILE: &str = "history.tsv";
/// The downloads folder in the home.
pub const DOWNLOADS_DIR: &str = "Downloads";
