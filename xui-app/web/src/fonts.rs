//! The fonts web pages are drawn with.
//!
//! The window's own text (menus, address bar, status bar) keeps the UI face
//! every xui app uses, Droid Sans. Pages get the Liberation fonts, registered
//! with Blitz by the `webfonts` crate. Serial evidence:
//! `WEB:FONTS:<faces loaded>/<faces looked for>`.

use xui_app::font;

/// Registers the UI fonts with the window's shaper and the web fonts with
/// Blitz.
pub fn register() {
    font::register_writer();
    let loaded = webfonts::register();
    println!("WEB:FONTS:{}/{}", loaded.faces, loaded.wanted);
}
