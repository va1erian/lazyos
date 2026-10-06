//! The window's keyboard shortcuts.

use xui_core::{Key, Modifiers};

use crate::app::Msg;
use crate::chrome::Command;

/// The window's keyboard shortcuts; Enter and Escape mean the address field
/// only while it has the focus, so a page's own forms still get them.
pub fn shortcut(key: Key, mods: Modifiers, in_address: bool) -> Option<Msg> {
    let menu = |command| Some(Msg::Menu(command));
    match key {
        Key::RETURN if in_address => Some(Msg::Go),
        Key::ESCAPE if in_address => Some(Msg::RestoreAddress),
        Key::ESCAPE => menu(Command::Stop),
        Key::LEFT if mods.alt => menu(Command::Back),
        Key::RIGHT if mods.alt => menu(Command::Forward),
        Key::HOME if mods.alt => menu(Command::Home),
        Key::F5 => menu(Command::Reload),
        Key::L if mods.ctrl => Some(Msg::FocusAddress),
        Key::H if mods.ctrl => menu(Command::ShowHistory),
        Key::J if mods.ctrl => menu(Command::ShowDownloads),
        Key::S if mods.ctrl => menu(Command::SavePage),
        Key::W if mods.ctrl => menu(Command::Close),
        _ => None,
    }
}
