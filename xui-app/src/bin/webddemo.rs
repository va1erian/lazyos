//! `webddemo` (`os.lazy.webddemo`): the client of webd's light views (docs/webd-plan.md, P2 slice 2).
//! It shows one light view: a blue page with a heading, a link, a form and a picture whose address
//! is a host the guest could reach but a light view must not (it has no network), and forwards the
//! pointer and the keyboard to the page. The logic is `xui_webd_client::demo` (host-tested against
//! the service half); the transport is `xui_app::platform::webd`.
//!
//! Ctrl+Q quits (a plain `q` is for the form), Ctrl+R makes a new view. When the view is lost the
//! app makes a new one once by itself and shows the page again.
//!
//! Serial evidence: `WEBDDEMO:UP:PASS` after the first frame of the window,
//! `WEBDDEMO:VIEW:PASS id=<n>`, `WEBDDEMO:FRAME:PASS colour=<c> w=<w> h=<h> seq=<n> ms=<ms>`,
//! `WEBDDEMO:LATENCY:<ms>`, `WEBDDEMO:LINK:<url>`, `WEBDDEMO:STATE:<title>`,
//! `WEBDDEMO:LOST:<reason>`, `WEBDDEMO:RECREATE:PASS`, `WEBDDEMO:<what>:FAIL:<why>` and
//! `WEBDDEMO:QUIT:PASS`.

#[path = "webddemo/keys.rs"]
mod keys;
#[path = "webddemo/screen.rs"]
mod screen;

use screen::Screen;
use xui_app::launch;
use xui_app::trace::{self, Channel};
use xui_core::arrange::build;
use xui_core::prelude::*;

/// The window size when a compositor lays the app out; as the display owner it fills the screen
/// instead.
const WINDOW: (i32, i32) = (480, 320);

#[derive(Clone)]
pub enum Msg {
    Recreate,
    Quit,
}

struct WebdDemo {
    screen: Handle<Screen>,
}

impl App for WebdDemo {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Recreate => self.screen.get().recreate(),
            Msg::Quit => {
                trace::pass!(Channel::Webddemo, "QUIT");
                ui.quit();
            }
        }
    }
}

fn main() {
    launch::run(Channel::Webddemo, "Web View Demo", WINDOW, |ui, backend| {
        backend.on_first_frame(|| trace::pass!(Channel::Webddemo, "UP"));
        let screen: Handle<Screen> = Handle::new();
        ui.root(
            column().children((build(|ui: &Ui<Msg>| Ok(Screen::new(ui)))
                .bind(&screen)
                .fill(1),)),
        )?;
        ui.on_key(|key, modifiers| match key {
            Key::Q if modifiers.ctrl => Some(Msg::Quit),
            Key::R if modifiers.ctrl => Some(Msg::Recreate),
            _ => None,
        });
        ui.on_close(|| Some(Msg::Quit));
        Ok(WebdDemo { screen })
    })
}
