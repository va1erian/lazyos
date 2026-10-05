//! `lazyweb`: LazyWeb, a basic web browser on the NetSurf browser core.
//!
//! A `xuid` desktop client. `lazyweb [URL]` opens the URL (an address as the
//! address bar takes it: `example.com` is `http://example.com/`), or the
//! built-in start page. Pages are laid out and drawn by NetSurf
//! (`xui-netsurf`); `http:` and `https:` are fetched by [`lazyweb::fetch`].
//! `app.rs` is the window; this file the LazyOS platform start-up.
//!
//! Serial evidence: `WEB:UP:PASS` after the first frame, the markers listed
//! in `app.rs`, and from `launch::run` `WEB:BIND:FAIL:<code>` when the display
//! cannot be bound and `WEB:RUN:FAIL:<error>` when the window cannot be built.

mod app;
mod page;

use std::rc::Rc;

use lazyweb::address;
use lazyweb::fetch::{self, Options};
use xui_app::launch;

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

fn main() {
    // Droid Sans (regular and bold), Droid Serif and JetBrains Mono: a page's
    // CSS families are drawn with these three (`xui_netsurf::FontFamilies`).
    xui_app::font::register_writer();
    xui_netsurf::set_font_families(xui_netsurf::FontFamilies {
        sans_serif: xui_app::font::UI_FAMILY.to_string(),
        serif: xui_app::font::SERIF_FAMILY.to_string(),
        monospace: xui_app::font::MONO_FAMILY.to_string(),
    });
    fetch::trace::now_ms();
    fetch::netsurf::install(Options::default());
    let url = url_arg();
    launch::run("WEB", "LazyWeb", WINDOW, move |ui, backend| {
        backend.set_size_hints(420, 300, 0, 0);
        backend.on_first_frame(|| println!("WEB:UP:PASS"));
        Browser::build(ui, Rc::clone(backend), url)
    })
}
