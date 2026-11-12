//! `writer`: LazyWriter, the word processor (issue #533).
//!
//! A `xuid` desktop client. The app itself (state, commands, file logic and
//! widget tree, ported from xui's wordpad example) is the portable
//! `xui-writer` crate; this file supplies the LazyOS platform: the backend,
//! the bundled fonts, the `AppContext` host (atomic writes, the pickers'
//! start folder), the printer remembered in the app's settings, the print
//! spooler (`printd`, over Messenger), a file named on the command line, and
//! the serial evidence the sessions grep for.
//!
//! Serial evidence: `WRITER:UP:PASS` after the first frame;
//! `WRITER:BIND:FAIL:<code>` when the display cannot be bound and
//! `WRITER:RUN:FAIL:<err>` when the loop fails. The crate reports
//! `WRITER:OPEN|SAVE|EXPORT|IMAGE:PASS|FAIL:<path>` as files are used and
//! `WRITER:PRINT:QUEUED:<pages>` once the print spooler has a job whole,
//! `WRITER:PRINT:PASS:<pages>` or `WRITER:PRINT:FAIL:<reason>` as it ends, and
//! `WRITER:QUIT:PASS` when the window closes.

use std::rc::Rc;

use serde::{Deserialize, Serialize};
use xui_app::trace::Channel;
use xui_host::settings::Settings;
use xui_writer::{Msg, Services};

/// Window size a compositor lays LazyWriter out at. The issue's 960x680 does
/// not fit the 1280x720 desktop: with xuid's title bar and border it is taller
/// than the 688 px above the taskbar, so the status bar ended up off screen.
const WINDOW: (i32, i32) = (960, 600);

/// What LazyWriter keeps in `user/<uid>/writer/..`.
#[derive(Default, Serialize, Deserialize)]
struct Prefs {
    /// The printer the last successful job went to.
    printer: Option<String>,
}

/// LazyOS's side beyond the host: the families `register_writer` registered,
/// the last printer (kept in the settings) and `printd`.
fn services(prefs: Rc<Settings<Prefs>>) -> Services {
    let mut services = Services::std();
    services.serif_family = xui_app::font::SERIF_FAMILY.to_owned();
    services.mono_family = xui_app::font::MONO_FAMILY.to_owned();
    let last = Rc::clone(&prefs);
    services.last_printer = Rc::new(move || last.get().printer.clone());
    services.remember_printer = Rc::new(move |printer| {
        // Losing the address only means typing it again next time.
        prefs.update(|p| p.printer = Some(printer.to_owned()));
    });
    services.print_queue = Rc::new(xui_app::platform::print::PrintService);
    services
}

fn main() {
    // Sans (regular and bold), Serif and Mono; no italic face, so italic is
    // synthesised by the shaper.
    xui_app::font::register_writer();
    xui_app::app(Channel::Writer, "LazyWriter")
        .size(WINDOW.0, WINDOW.1)
        // The editor re-flows its text to the window width.
        .min_size(480, 320)
        .opens(Msg::OpenChosen)
        .run(|ui, cx| {
            let prefs = Rc::new(cx.settings::<Prefs>());
            xui_writer::ui::build(ui, cx.host(), services(prefs))
        })
}
