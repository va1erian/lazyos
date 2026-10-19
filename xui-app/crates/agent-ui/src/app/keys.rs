//! The window-level key hook: what Enter, Esc and the Ctrl shortcuts mean.

use std::cell::Cell;

use xui_core::message::Key;

use super::Page;
use crate::Msg;

/// Which field the user typed in last: Enter sends only from the composer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Typing {
    Composer,
    Other,
}

/// What the window-level key hook may look at.
pub(super) struct Shared {
    pub(super) page: Cell<Page>,
    pub(super) typing: Cell<Typing>,
    pub(super) busy: Cell<bool>,
}

/// The window-level shortcuts.
pub(super) fn shortcut(
    shared: &Shared,
    key: Key,
    modifiers: xui_core::message::Modifiers,
) -> Option<Msg> {
    let plain = !modifiers.ctrl && !modifiers.alt;
    match (shared.page.get(), key) {
        (Page::Chat, Key::RETURN)
            if plain && !modifiers.shift && shared.typing.get() == Typing::Composer =>
        {
            Some(Msg::Send)
        }
        (Page::Key, Key::RETURN) if plain => Some(Msg::KeySubmit),
        (Page::Chat, Key::ESCAPE) if shared.busy.get() => Some(Msg::Stop),
        (Page::Settings, Key::ESCAPE) => Some(Msg::Back),
        (Page::Chat, Key::N) if modifiers.ctrl => Some(Msg::NewChat),
        (Page::Chat, k) if modifiers.ctrl && k.code() == 0xBC => Some(Msg::OpenSettings),
        _ => None,
    }
}
