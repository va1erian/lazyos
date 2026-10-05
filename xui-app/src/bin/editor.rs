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

use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_app::themed::run_themed;
use xui_core::backend::PlatformSpec;
use xui_core::units::Dip;

/// Window size a compositor lays the editor out at.
const WINDOW: (i32, i32) = (900, 640);

fn main() -> std::process::ExitCode {
    // The grid needs a real monospace face; `monospace` alone would fall back
    // to the proportional UI font and letter-space the text.
    xui_app::font::register_mono();
    let path = xui_app::platform::argv::file_arg(std::env::args_os());
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("EDITOR:BIND:FAIL:{code}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    // The editor re-flows its text area and line numbers to the window size.
    backend.set_size_hints(400, 300, 0, 0);
    backend.on_first_frame(|| println!("EDITOR:UP:PASS"));

    let spec = PlatformSpec::new("Editor").size(Dip(width as f32), Dip(height as f32));
    let outcome = run_themed(&backend, spec, move |ui| {
        let mut notepad = ui::build(ui).expect("the notepad's widgets built");
        if let Some(path) = path {
            commands::open_path(&mut notepad, ui, path);
        }
        // Title the window ("Untitled - Editor") before the first frame.
        commands::refresh(&mut notepad, ui);
        notepad
    });
    backend.unbind();
    std::process::ExitCode::from(xui_app::launch::finish("EDITOR", outcome))
}
