//! `xui-docs`: renders Markdown files on a Blitz view.
//!
//! A `xuid` desktop client (or the display owner in a headless session). With no
//! path argument it shows a built-in welcome page; a document is opened with the
//! toolbar's Open button or `Ctrl+O`. Blitz lays the page out on its own
//! engine thread; `app.rs` is the window, this file the LazyOS platform start-up.
//!
//! Serial evidence: `DOCS:UP:PASS` after the first frame, plus the markers
//! documented in `app.rs`; `DOCS:BIND:FAIL:<code>` when the display cannot be
//! bound.

mod app;

use std::path::PathBuf;
use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_app::themed::run_themed;
use xui_core::backend::PlatformSpec;
use xui_core::units::Dip;
use xui_docs::page;

use app::Docs;

/// Window size a compositor lays the page out at.
const WINDOW: (i32, i32) = (900, 640);

/// Shown when no file is named on the command line.
const WELCOME: &str = include_str!("welcome.md");

fn main() -> std::process::ExitCode {
    xui_app::font::register_docs();
    webfonts::register();
    let path = xui_app::platform::argv::file_arg(std::env::args_os());
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("DOCS:BIND:FAIL:{code}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    backend.on_first_frame(|| println!("DOCS:UP:PASS"));

    let spec = PlatformSpec::new("Docs").size(Dip(width as f32), Dip(height as f32));
    let outcome = run_themed(&backend, spec, move |ui| {
        // The welcome page first; a named file is then opened like any other, so
        // a bad path shows the same error page (and `DOCS:OPEN:FAIL`) as the
        // dialog does instead of ending the app.
        let mut docs =
            Docs::build(ui, page(WELCOME), None::<PathBuf>).expect("the Docs window was created");
        if let Some(path) = path {
            docs.open(ui, path);
        }
        docs
    });
    backend.unbind();
    std::process::ExitCode::from(xui_app::launch::finish("DOCS", outcome))
}
