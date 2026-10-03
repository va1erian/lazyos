//! `writer`: LazyWriter, the word processor (issue #533).
//!
//! A `xuid` desktop client. The app itself (state, commands, file logic and
//! widget tree, ported from xui's wordpad example) is the portable
//! `xui-writer` crate; this file supplies the LazyOS platform: the backend,
//! the bundled fonts, atomic writes, the pickers' start folder, a file named
//! on the command line, and the serial evidence the sessions grep for.
//!
//! Serial evidence: `WRITER:UP:PASS` after the first frame;
//! `WRITER:BIND:FAIL:<code>` when the display cannot be bound and
//! `WRITER:RUN:FAIL:<err>` when the loop fails. The crate prints
//! `WRITER:OPEN|SAVE|EXPORT|IMAGE:PASS|FAIL:<path>` as files are used.

use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_core::app::run_app;
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::units::Dip;
use xui_writer::Host;

/// Window size a compositor lays LazyWriter out at. The issue's 960x680 does
/// not fit the 1280x720 desktop: with xuid's title bar and border it is taller
/// than the 688 px above the taskbar, so the status bar ended up off screen.
const WINDOW: (i32, i32) = (960, 600);

/// LazyOS's side of the app: atomic writes, `$HOME` (or `/transient`) and the
/// families `register_writer` registered.
fn host() -> Host {
    let mut host = Host::std(xui_app::platform::dirs::default_dir());
    host.write = Rc::new(xui_app::platform::storage::write_atomic);
    host.serif_family = xui_app::font::SERIF_FAMILY.to_owned();
    host.mono_family = xui_app::font::MONO_FAMILY.to_owned();
    host
}

fn main() -> std::process::ExitCode {
    // Sans (regular and bold), Serif and Mono; no italic face, so italic is
    // synthesised by the shaper.
    xui_app::font::register_writer();
    let path = xui_app::platform::argv::file_arg(std::env::args_os());
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("WRITER:BIND:FAIL:{code}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    // The editor re-flows its text to the window width.
    backend.set_size_hints(480, 320, 0, 0);
    backend.on_first_frame(|| println!("WRITER:UP:PASS"));

    // Match the desktop's light/dark mode and accent (Settings).
    let theme = backend.desktop_theme();
    let spec = PlatformSpec::new("LazyWriter").size(Dip(width as f32), Dip(height as f32));
    let outcome = run_app(Rc::clone(&backend) as Rc<dyn Backend>, spec, move |ui| {
        if let Some(theme) = theme {
            ui.set_theme(theme);
        }
        let mut writer = xui_writer::ui::build(ui, host()).expect("LazyWriter's widgets built");
        if let Some(path) = path {
            xui_writer::commands::files::open_path(&mut writer, ui, path);
        }
        writer
    });
    backend.unbind();
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            println!("WRITER:RUN:FAIL:{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
