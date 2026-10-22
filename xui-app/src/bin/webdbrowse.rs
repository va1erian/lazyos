//! `webdbrowse` (`os.lazy.webdbrowse`, "Web View Test"): the test client of webd's full views
//! (docs/webd-plan.md, P2 slice 3). It runs `xui_webd_client::browse` (host-tested against the
//! service half and a fake web): view A through `os.lazy.webd.browser.v1`, views B to D through the
//! net and view scopes, against the harness server at the first `http://` argument (default
//! `http://10.0.2.2:18081`, the host as QEMU's user network shows it). The window paints view A's
//! picture with the last step under it, and forwards the pointer, the wheel and the keyboard to view A.
//!
//! Ctrl+Q quits (plain keys go to the page).
//!
//! Serial evidence: `WEBDBROWSE:UP:PASS` after the first frame of the window, then one
//! `WEBDBROWSE:<STEP>:PASS:<detail>` or `WEBDBROWSE:<STEP>:FAIL:<why>` per step (`VIEW`, `BLOCKLIST`,
//! `BLOCKED`, `UNBLOCKED`, `NAV`, `BACK`, `FORWARD`, `COOKIE_A`, `COOKIE_B`, `COOKIE_A2`, `DOWNLOAD`,
//! `DIALOG`, `POPUP`, `HELD`, `SCOPE`, `DELTA`, `LATENCY`), `WEBDBROWSE:DONE:PASS|FAIL` and
//! `WEBDBROWSE:QUIT:PASS`.

#[path = "webdbrowse/screen.rs"]
mod screen;

use screen::Screen;
use xui_app::launch;
use xui_app::trace::{self, Channel};
use xui_core::arrange::build;
use xui_core::prelude::*;

/// The window size when a compositor lays the app out; as the display owner it fills the screen
/// instead.
const WINDOW: (i32, i32) = (800, 600);

#[derive(Clone)]
pub enum Msg {
    Quit,
}

struct Webdbrowse {
    _screen: Handle<Screen>,
}

impl App for Webdbrowse {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Quit => {
                trace::pass!(Channel::Webdbrowse, "QUIT");
                ui.quit();
            }
        }
    }
}

fn main() {
    launch::run(Channel::Webdbrowse, "Web View Test", WINDOW, |ui, backend| {
        backend.on_first_frame(|| trace::pass!(Channel::Webdbrowse, "UP"));
        let screen: Handle<Screen> = Handle::new();
        ui.root(
            column().children((build(|ui: &Ui<Msg>| Ok(Screen::new(ui)))
                .bind(&screen)
                .fill(1),)),
        )?;
        ui.on_key(|key, modifiers| match key {
            Key::Q if modifiers.ctrl => Some(Msg::Quit),
            _ => None,
        });
        ui.on_close(|| Some(Msg::Quit));
        Ok(Webdbrowse { _screen: screen })
    })
}
