//! Keyboard navigation of the shell's panels (issue #648): the start menu,
//! its category submenus, a tray item's menu, and the tray itself (Win+B).
//!
//! A panel never takes keyboard focus in `xuid`, so while one of them is
//! open the shell holds the compositor's panel-key grab ([`sync`],
//! `GrabPanelKeys`) and every key arrives as a `PanelKey` shell event. The
//! key goes to the innermost open level: a tray menu (its deepest panel),
//! else the start menu's submenu when the keyboard moved into it, else the
//! start menu, else the tray. The rules are `lazyshell::keynav` (host
//! tested); each panel carries out the step on its own rows. The pointer
//! path is unchanged.
//!
//! Serial: `SHELL:KEYS:GRAB on|off`, `SHELL:TRAY:KEYS app=<id>` (the tray
//! cell the keyboard is on) and `SHELL:TRAY:KEYS:LEAVE`; each panel prints
//! its own `...:KEY:...` markers.

use std::rc::Rc;

use lazyshell::keynav::{self, NavKey, TrayStep};
use xui_core::app::Ui;

use super::ctx::Ctx;
use super::menu::{self, MenuMsg};
use super::submenu::SubMsg;
use super::tray;
use super::tray::input::Input;
use crate::sys::key;

/// F10, for Shift+F10 (the context-menu chord).
const F10: u32 = key::F1 + 9;

/// The navigation key `raw` (a forwarded key with modifier bits) means;
/// `None` for anything else, which the panels ignore.
pub fn nav_key(raw: u32) -> Option<NavKey> {
    let code = raw & key::CODE_MASK;
    if raw & (key::MOD_CTRL | key::MOD_ALT) != 0 {
        return None;
    }
    Some(match code {
        key::UP => NavKey::Up,
        key::DOWN => NavKey::Down,
        key::LEFT => NavKey::Left,
        key::RIGHT => NavKey::Right,
        key::HOME => NavKey::Home,
        key::END => NavKey::End,
        key::ENTER | key::SPACE => NavKey::Enter,
        key::ESCAPE => NavKey::Escape,
        F10 if raw & key::MOD_SHIFT != 0 => NavKey::Menu,
        _ => return None,
    })
}

/// Hold the panel-key grab exactly while a panel menu or the tray has the
/// keyboard.
pub fn sync(ctx: &Ctx) {
    let want = ctx.menu_window.borrow().is_some()
        || !ctx.tray.menus.borrow().is_empty()
        || ctx.tray.keys.get().is_some();
    if ctx.panel_grab.replace(want) == want {
        return;
    }
    match ctx.client.grab_panel_keys(want) {
        Ok(()) => println!("SHELL:KEYS:GRAB {}", if want { "on" } else { "off" }),
        Err(code) => {
            // An older compositor: the panels stay pointer-only.
            ctx.panel_grab.set(false);
            ctx.note("panel-keys", || {
                format!("SHELL:KEYS:GRAB:FAIL err={}", -code)
            });
        }
    }
}

/// A `PanelKey` event: hand it to the innermost open level.
pub fn on_key<M: 'static>(ctx: &Rc<Ctx>, ui: &Ui<M>, raw: u32) {
    let Some(key) = nav_key(raw) else {
        return;
    };
    if tray::menu::key(ctx, key) {
        return;
    }
    if ctx.submenu_window.borrow().is_some() && ctx.keys_in_submenu.get() {
        if let Some(sub) = &*ctx.submenu_window.borrow() {
            sub.send(SubMsg::Key(key));
        }
        return;
    }
    if let Some(menu) = &*ctx.menu_window.borrow() {
        menu.send(MenuMsg::Key(key));
        return;
    }
    if ctx.tray.keys.get().is_some() {
        tray_key(ctx, ui, key);
    }
}

/// Win+B (`TrayKeys`): close the start menu and light the first tray cell.
pub fn on_tray_keys(ctx: &Rc<Ctx>) {
    menu::close(ctx);
    tray::menu::close(ctx);
    let first = ctx
        .tray
        .layout
        .borrow()
        .cells
        .first()
        .map(|cell| cell.app.clone());
    match first {
        Some(app) => {
            ctx.tray.keys.set(Some(0));
            println!("SHELL:TRAY:KEYS app={app}");
        }
        None => println!("SHELL:TRAY:KEYS:EMPTY"),
    }
    ctx.repaint_bar();
    sync(ctx);
}

/// Leave the tray (Escape, a click elsewhere, the start menu).
pub fn leave_tray(ctx: &Ctx) {
    if ctx.tray.keys.take().is_some() {
        println!("SHELL:TRAY:KEYS:LEAVE");
        ctx.repaint_bar();
        sync(ctx);
    }
}

/// A key while the tray has the keyboard.
fn tray_key<M: 'static>(ctx: &Rc<Ctx>, ui: &Ui<M>, key: NavKey) {
    let cells = ctx.tray.layout.borrow().cells.len();
    match keynav::tray_step(cells, ctx.tray.keys.get(), key) {
        TrayStep::Select(index) => {
            ctx.tray.keys.set(Some(index));
            if let Some(cell) = ctx.tray.layout.borrow().cells.get(index) {
                println!("SHELL:TRAY:KEYS app={}", cell.app);
            }
            ctx.repaint_bar();
        }
        TrayStep::Activate(index) => tray::input::on_cell(ctx, ui, index, Input::Primary),
        TrayStep::Menu(index) => {
            tray::input::on_cell(ctx, ui, index, Input::Secondary);
            // A menu opened from the keyboard starts on its first row.
            tray::menu::key(ctx, NavKey::Home);
        }
        TrayStep::Leave => leave_tray(ctx),
        TrayStep::Nothing => {}
    }
    sync(ctx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrows_enter_space_and_escape_are_navigation_keys() {
        assert_eq!(nav_key(key::UP), Some(NavKey::Up));
        assert_eq!(nav_key(key::DOWN | key::MOD_SHIFT), Some(NavKey::Down));
        assert_eq!(nav_key(key::ENTER), Some(NavKey::Enter));
        assert_eq!(nav_key(key::SPACE), Some(NavKey::Enter));
        assert_eq!(nav_key(key::ESCAPE), Some(NavKey::Escape));
        assert_eq!(nav_key(F10 | key::MOD_SHIFT), Some(NavKey::Menu));
    }

    #[test]
    fn chords_and_text_are_not() {
        assert_eq!(nav_key(key::UP | key::MOD_CTRL), None);
        assert_eq!(nav_key(key::LEFT | key::MOD_ALT), None);
        assert_eq!(nav_key(F10), None);
        assert_eq!(nav_key(b'a' as u32), None);
    }
}
