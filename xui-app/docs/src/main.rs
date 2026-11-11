//! `xui-docs`: shows Markdown files in xui's read-only Markdown view (`xui-markdown`).
//!
//! A `xuid` desktop client (or the display owner in a headless session). With no
//! path argument it shows a built-in welcome page; a document is opened with the
//! toolbar's Open button or `Ctrl+O`. `app.rs` is the window, this file the LazyOS platform
//! start-up.
//!
//! Serial evidence: `DOCS:UP:PASS` after the first frame, plus the markers
//! documented in `app.rs`; `DOCS:BIND:FAIL:<code>` when the display cannot be
//! bound.

mod app;
mod loader;

use xui_app::trace::Channel;

use app::{Docs, Msg};

/// Window size a compositor lays the page out at.
const WINDOW: (i32, i32) = (900, 640);

/// Shown when no file is named on the command line.
const WELCOME: &str = include_str!("welcome.md");

fn main() {
    xui_app::font::register_docs();
    xui_app::app(Channel::Docs, "Docs")
        .size(WINDOW.0, WINDOW.1)
        // The welcome page first; a named file is then opened like any other,
        // so a bad path shows the same error page (and `DOCS:OPEN:FAIL`) as
        // the dialog does instead of ending the app.
        .opens(Msg::OpenChosen)
        .run(|ui, _| Docs::build(ui, WELCOME))
}
