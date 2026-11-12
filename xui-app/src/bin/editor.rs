//! `xui-editor`: the text editor, migrated from xui's notepad example.
//!
//! A `xuid` desktop client (or the display owner, for a headless CI session).
//! The widget tree, commands and find/replace session live in `editor/`; this
//! file supplies the LazyOS platform: the backend, a file named on the command
//! line, and the serial evidence markers the screenshot sessions grep for.
//!
//! Serial evidence: `EDITOR:UP:PASS` after the first frame, `EDITOR:OPEN:PASS`
//! after a file loads, `EDITOR:SAVE:PASS` after a successful save,
//! `EDITOR:OPEN:FAIL:<path>` / `EDITOR:SAVE:FAIL:<path>` on failure.

// A binary crate root in `src/bin/editor.rs` resolves `mod app;` under
// `src/bin/`, so the notepad's submodules are named explicitly.
#[path = "editor/app.rs"]
mod app;
#[path = "editor/commands/mod.rs"]
mod commands;
#[path = "editor/ui.rs"]
mod ui;

use app::Msg;

use xui_app::trace::Channel;

/// Window size a compositor lays the editor out at.
const WINDOW: (i32, i32) = (900, 640);

fn main() {
    // The grid needs a real monospace face; `monospace` alone would fall back
    // to the proportional UI font and letter-space the text.
    xui_app::font::register_mono();
    xui_app::app(Channel::Editor, "Editor")
        .size(WINDOW.0, WINDOW.1)
        // The editor re-flows its text area and line numbers to the window size.
        .min_size(400, 300)
        .opens(Msg::OpenChosen)
        .run(|ui, cx| {
            let mut notepad = ui::build(ui, cx)?;
            // Title the window ("Untitled - Editor") before the first frame.
            commands::refresh(&mut notepad, ui);
            Ok(notepad)
        })
}
