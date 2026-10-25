//! `cursored`: Cursor Studio, draw the mouse pointer and apply it.
//!
//! The portable window lives in `crates/cursored`; this file supplies the
//! platform: a [`ConfdStore`] over `os.lazy.confd`, into whose per-user
//! `ui/cursor/theme` key `Apply` writes the theme, which `xuid` follows live.
//!
//! Serial evidence: `CURSORED:UP:PASS` after the first frame,
//! `CURSORED:APPLY:PASS` when a theme was written, `CURSORED:REFUSED:FAIL`
//! when it was refused, `CURSORED:QUIT:PASS` on `q` (or the close button).

use std::rc::Rc;

use xui_app::launch;
use xui_app::platform::confd_store::ConfdStore;
use xui_app::trace::{self, Channel};
use xui_core::prelude::*;
use xui_cursored::app::{evidence, Studio, WINDOW};
use xui_cursored::store::ThemeStore;
use xui_cursored::Msg;
use xui_settings::store::{ConfigStore, Value};

/// The theme store over the real `confd`.
struct Confd(ConfdStore);

impl ThemeStore for Confd {
    fn uid(&self) -> Option<u32> {
        self.0.uid()
    }

    fn get(&self, key: &str) -> Option<Value> {
        self.0.get(key)
    }

    fn set(&self, key: &str, value: Value) -> std::result::Result<(), String> {
        self.0.set(key, value)
    }
}

fn main() {
    evidence::install(|step, detail| match step {
        "APPLY" => trace::pass!(Channel::Cursored, "APPLY"),
        "QUIT" => trace::pass!(Channel::Cursored, "QUIT"),
        _ => trace::fail!(Channel::Cursored, "REFUSED", "{detail}"),
    });
    launch::run(Channel::Cursored, "Cursor Studio", WINDOW, |ui, backend| {
        backend.on_first_frame(|| trace::pass!(Channel::Cursored, "UP"));
        let studio = Studio::build(ui, Rc::new(Confd(ConfdStore::new())))?;
        ui.on_key(|key, modifiers| (key == Key::Q && modifiers.ctrl).then_some(Msg::Quit));
        ui.on_close(|| Some(Msg::Quit));
        Ok(studio)
    })
}
