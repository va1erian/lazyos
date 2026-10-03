//! `lazyweb`: LazyWeb, a basic web browser on the NetSurf browser core.
//!
//! A `xuid` desktop client. `lazyweb [URL]` opens the URL (an address as the
//! address bar takes it: `example.com` is `http://example.com/`), or the
//! built-in start page. Pages are laid out and drawn by NetSurf
//! (`xui-netsurf`); `http:` and `https:` are fetched by [`lazyweb::fetch`].
//! `app.rs` is the window; this file the LazyOS platform start-up.
//!
//! Serial evidence: `WEB:UP:PASS` after the first frame, the markers listed
//! in `app.rs`, and `WEB:BIND:FAIL:<code>` when the display cannot be bound.

mod app;

use std::rc::Rc;

use lazyweb::address;
use lazyweb::fetch::{self, Options};
use xui_app::backend::LazyOSBackend;
use xui_app::themed::run_themed;
use xui_core::backend::PlatformSpec;
use xui_core::units::Dip;

use app::Browser;

/// The window size asked of the compositor.
const WINDOW: (i32, i32) = (1000, 700);

/// The first argument that is not an option, as an address.
fn url_arg() -> Option<String> {
    std::env::args()
        .skip(1)
        .find(|arg| !arg.starts_with('-') && !arg.starts_with("attempt="))
        .and_then(|arg| address::normalize(&arg))
}

fn main() -> std::process::ExitCode {
    // Droid Sans (regular and bold), Droid Serif and JetBrains Mono: a page's
    // CSS families are drawn with these three (`xui_netsurf::FontFamilies`).
    xui_app::font::register_writer();
    xui_netsurf::set_font_families(xui_netsurf::FontFamilies {
        sans_serif: xui_app::font::UI_FAMILY.to_string(),
        serif: xui_app::font::SERIF_FAMILY.to_string(),
        monospace: xui_app::font::MONO_FAMILY.to_string(),
    });
    fetch::netsurf::install(Options::default());
    let url = url_arg();
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("WEB:BIND:FAIL:{code}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    backend.set_size_hints(420, 300, 0, 0);
    backend.on_first_frame(|| println!("WEB:UP:PASS"));

    let spec = PlatformSpec::new("LazyWeb").size(Dip(width as f32), Dip(height as f32));
    let shared = Rc::clone(&backend);
    let outcome = run_themed(&backend, spec, move |ui| {
        Browser::build(ui, shared, url).expect("the LazyWeb window was created")
    });
    backend.unbind();
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            println!("WEB:RUN:FAIL:{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
