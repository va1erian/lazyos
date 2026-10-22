//! `webdattack` (`os.lazy.webdattack`, "Web Attack Probe"): the **malicious client** half of webd's
//! attack harness (`tools/webd/attack_run.py`, docs/webd-plan.md P5). It is a real client of webd
//! whose manifest lists **only** `os.lazy.webd.view.v1` (not the `net` or `browser` scope), so a
//! `CreateView` it sends through a scope it was not granted is refused by the kernel before webd
//! sees it — the gate the host fabric cannot reproduce. Once its window is up it runs the probes of
//! `xui_webd_client::attack` against the running webd, prints one line each, and quits.
//!
//! Serial evidence (channel `WEBDATTACK`): `WEBDATTACK:UP:PASS` on the first frame, then one
//! `WEBDATTACK:PROBE:<name>:<outcome>` per probe (`OK`, `REFUSED:<errno>`, `REFUSED:<errno>:<count>`,
//! `NOTRUNNING`, `LOST`, `NOVIEW`), `WEBDATTACK:DONE:PASS` and `WEBDATTACK:QUIT:PASS`. The judge
//! (`tools/webd/attackjudge.py`) holds the expectation table.

#[path = "webdattack/screen.rs"]
mod screen;

use screen::Screen;
use xui_app::launch;
use xui_app::trace::{self, Channel};
use xui_core::arrange::build;
use xui_core::prelude::*;

/// The window size when a compositor lays the app out.
const WINDOW: (i32, i32) = (320, 240);

#[derive(Clone)]
pub enum Msg {
    Quit,
}

struct WebdAttack {
    _screen: Handle<Screen>,
}

impl App for WebdAttack {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Quit => {
                trace::pass!(Channel::Webdattack, "QUIT");
                ui.quit();
            }
        }
    }
}

fn main() {
    launch::run(
        Channel::Webdattack,
        "Web Attack Probe",
        WINDOW,
        |ui, backend| {
            backend.on_first_frame(|| trace::pass!(Channel::Webdattack, "UP"));
            let screen: Handle<Screen> = Handle::new();
            ui.root(
                column().children((build(|ui: &Ui<Msg>| Ok(Screen::new(ui)))
                    .bind(&screen)
                    .fill(1),)),
            )?;
            ui.on_close(|| Some(Msg::Quit));
            Ok(WebdAttack { _screen: screen })
        },
    )
}
