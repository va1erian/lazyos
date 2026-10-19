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

/// Keeps `typing` honest: a message from any control but the composer means
/// the focus is not in the composer, so Enter must not send its draft.
pub(super) fn note_focus(shared: &Shared, msg: &Msg) {
    match msg {
        Msg::ComposerText(_) => shared.typing.set(Typing::Composer),
        Msg::Tick
        | Msg::Report(_)
        | Msg::Send
        | Msg::SendText(_)
        | Msg::EditLast
        | Msg::NewChat
        | Msg::Accept
        | Msg::KeySubmit
        | Msg::KeyEntered(_)
        | Msg::Back
        | Msg::ForgetKey
        | Msg::LinkChoice(_)
        | Msg::Quit => {}
        _ => shared.typing.set(Typing::Other),
    }
}

/// The window-level shortcuts.
pub(super) fn shortcut(
    shared: &Shared,
    key: Key,
    modifiers: xui_core::message::Modifiers,
) -> Option<Msg> {
    let plain = !modifiers.ctrl && !modifiers.alt;
    if key == Key::TAB {
        // Focus moves on: Enter now belongs to whatever has it.
        shared.typing.set(Typing::Other);
    }
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

#[cfg(test)]
mod tests {
    use xui_core::message::Modifiers;

    use super::*;

    fn shared(page: Page) -> Shared {
        Shared {
            page: Cell::new(page),
            typing: Cell::new(Typing::Composer),
            busy: Cell::new(false),
        }
    }

    #[test]
    fn enter_sends_only_from_the_composer_on_the_chat_page() {
        let s = shared(Page::Chat);
        assert!(matches!(
            shortcut(&s, Key::RETURN, Modifiers::NONE),
            Some(Msg::Send)
        ));
        // Shift+Enter is a new line.
        let shift = Modifiers {
            shift: true,
            ..Modifiers::NONE
        };
        assert!(shortcut(&s, Key::RETURN, shift).is_none());
        // Tab moves the focus on: Enter now belongs to the focused control.
        assert!(shortcut(&s, Key::TAB, Modifiers::NONE).is_none());
        assert!(shortcut(&s, Key::RETURN, Modifiers::NONE).is_none());
        // Typing in the composer takes it back.
        note_focus(&s, &Msg::ComposerText("x".into()));
        assert!(shortcut(&s, Key::RETURN, Modifiers::NONE).is_some());
        // A press on a button or the list hands it away again.
        note_focus(&s, &Msg::Select(0));
        assert!(shortcut(&s, Key::RETURN, Modifiers::NONE).is_none());
        note_focus(&s, &Msg::Tick);
        assert!(shortcut(&s, Key::RETURN, Modifiers::NONE).is_none());
    }

    #[test]
    fn escape_stops_a_busy_chat_and_leaves_the_settings() {
        let s = shared(Page::Chat);
        assert!(shortcut(&s, Key::ESCAPE, Modifiers::NONE).is_none());
        s.busy.set(true);
        assert!(matches!(
            shortcut(&s, Key::ESCAPE, Modifiers::NONE),
            Some(Msg::Stop)
        ));
        s.page.set(Page::Settings);
        assert!(matches!(
            shortcut(&s, Key::ESCAPE, Modifiers::NONE),
            Some(Msg::Back)
        ));
    }
}
