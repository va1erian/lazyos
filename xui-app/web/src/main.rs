//! `lazyweb`: LazyWeb, a web browser on the NetSurf browser core.
//!
//! A `xuid` desktop client. `lazyweb [URL]` opens the URL (an address as the
//! address bar takes it: `example.com` is `http://example.com/`), or the
//! built-in start page; `init` starts it with a URL when another app opens a
//! link (`mimed` types `https://...` as `x-scheme-handler/https`). Pages are
//! laid out and drawn by NetSurf (`xui-netsurf`); `http:` and `https:` are
//! fetched by [`lazyweb::fetch`]. The history is kept in the app's folder in
//! the user's home and downloads go to `$HOME/Downloads`.
//!
//! `app.rs` is the window, `chrome.rs` its layout; this file the LazyOS
//! platform start-up.
//!
//! Serial evidence: `WEB:UP:PASS` after the first frame, the markers listed
//! in `app.rs`, and from `launch::run` `WEB:BIND:FAIL:<code>` when the display
//! cannot be bound and `WEB:RUN:FAIL:<error>` when the window cannot be built.

mod app;
mod chrome;
mod indicators;
mod internal;
mod page;
mod transfers;

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use lazyweb::address;
use lazyweb::downloads::Saver;
use lazyweb::fetch::{self, Options};
use lazyweb::visits::Visits;
use xui_app::launch;

use app::{Browser, Setup};
use transfers::Transfers;

/// The window size asked of the compositor.
const WINDOW: (i32, i32) = (1000, 700);

/// The app's id: its data folder is `$HOME/.apps/<SYSTEM_NAME>`.
const SYSTEM_NAME: &str = "os.lazy.lazyweb";
/// The history file in that folder.
const HISTORY_FILE: &str = "history.tsv";
/// The downloads folder in the home.
const DOWNLOADS_DIR: &str = "Downloads";

/// The first argument that is not an option, as an address.
fn url_arg() -> Option<String> {
    std::env::args()
        .skip(1)
        .find(|arg| !arg.starts_with('-') && !arg.starts_with("attempt="))
        .and_then(|arg| address::normalize(&arg))
}

/// The user's home, when there is one.
fn home() -> Option<String> {
    std::env::var("HOME").ok().filter(|h| h.starts_with('/'))
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

    // Without a home nothing is kept: the history lives in memory and
    // downloads go to the scratch volume.
    let home = home();
    let visits = match &home {
        Some(home) => {
            Visits::open(PathBuf::from(fhs::app_data_dir(home, SYSTEM_NAME)).join(HISTORY_FILE))
        }
        None => Visits::in_memory(),
    };
    let folder = match &home {
        Some(home) => PathBuf::from(home).join(DOWNLOADS_DIR),
        None => PathBuf::from(fhs::mount::TMP),
    };
    let saver = Saver::new(folder.clone());
    let transfers = Transfers::new(saver.destinations(), folder);
    xui_netsurf::set_downloader(Arc::new(saver));

    let setup = Setup {
        url: url_arg(),
        visits,
        transfers,
    };
    launch::run("WEB", "LazyWeb", WINDOW, move |ui, backend| {
        backend.set_size_hints(420, 300, 0, 0);
        backend.on_first_frame(|| println!("WEB:UP:PASS"));
        Browser::build(ui, Rc::clone(backend), setup)
    })
}
