#![forbid(unsafe_code)]

//! The window's keyboard shortcuts, as a pure function of the key, its
//! modifiers and whether the address bar has the focus: while the user types
//! an address, the keys that edit text (Delete, Backspace, Ctrl+C, Ctrl+V)
//! stay with the field, and Return and Escape mean Go and Revert.

use std::time::Duration;

use xui_core::message::{Key, Modifiers};

use super::Msg;

/// How long after an Alt+arrow shortcut a selection change counts as the
/// view's own move for that arrow. `Ui::on_key` runs the shortcut first and
/// cannot consume the key, so the focused view then moves its selection one
/// step from the one the navigation made, in the same event. A later click
/// is never that close.
pub(super) const ARROW_ECHO: Duration = Duration::from_millis(100);

/// Whether a view would also move its selection for `key`.
pub(super) fn is_arrow(key: Key) -> bool {
    matches!(key, Key::LEFT | Key::RIGHT | Key::UP | Key::DOWN)
}

/// The keys the window looks at; every other key goes to the focused widget
/// without a message.
pub(super) fn watched(key: Key, modifiers: Modifiers) -> bool {
    matches!(
        key,
        Key::RETURN | Key::ESCAPE | Key::DELETE | Key::BACK | Key::F4 | Key::F5
    ) || (modifiers.alt && matches!(key, Key::LEFT | Key::RIGHT | Key::UP | Key::D))
        || (modifiers.ctrl && matches!(key, Key::C | Key::V | Key::L))
}

/// The message a watched key raises, if any.
pub(super) fn shortcut(key: Key, modifiers: Modifiers, in_address: bool) -> Option<Msg> {
    if modifiers.alt {
        return match key {
            Key::LEFT => Some(Msg::Back),
            Key::RIGHT => Some(Msg::Forward),
            Key::UP => Some(Msg::Up),
            Key::D => Some(Msg::FocusAddress),
            Key::RETURN if !in_address => Some(Msg::Properties),
            _ => None,
        };
    }
    match key {
        Key::F5 => Some(Msg::Refresh),
        Key::F4 => Some(Msg::FocusAddress),
        Key::L if modifiers.ctrl => Some(Msg::FocusAddress),
        Key::RETURN if in_address => Some(Msg::Go),
        Key::ESCAPE if in_address => Some(Msg::RestoreAddress),
        _ if in_address => None,
        Key::BACK => Some(Msg::Up),
        Key::DELETE => Some(Msg::Delete),
        Key::C if modifiers.ctrl => Some(Msg::Copy),
        Key::V if modifiers.ctrl => Some(Msg::Paste),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use xui_core::message::{Key, Modifiers};

    use super::{shortcut, watched};
    use crate::window::Msg;

    const PLAIN: Modifiers = Modifiers::NONE;
    const CTRL: Modifiers = Modifiers {
        ctrl: true,
        ..PLAIN
    };
    const ALT: Modifiers = Modifiers { alt: true, ..PLAIN };

    fn name(msg: Option<Msg>) -> String {
        msg.map(|msg| format!("{msg:?}")).unwrap_or_default()
    }

    #[test]
    fn editing_keys_stay_with_the_address_bar() {
        for (key, modifiers) in [
            (Key::DELETE, PLAIN),
            (Key::BACK, PLAIN),
            (Key::C, CTRL),
            (Key::V, CTRL),
        ] {
            assert!(watched(key, modifiers));
            assert!(shortcut(key, modifiers, false).is_some());
            assert!(
                shortcut(key, modifiers, true).is_none(),
                "{key:?} edits the address"
            );
        }
    }

    #[test]
    fn return_and_escape_mean_go_and_revert_only_in_the_address_bar() {
        assert_eq!(name(shortcut(Key::RETURN, PLAIN, true)), "Go");
        assert_eq!(name(shortcut(Key::ESCAPE, PLAIN, true)), "RestoreAddress");
        assert!(
            shortcut(Key::RETURN, PLAIN, false).is_none(),
            "the view opens"
        );
        assert!(shortcut(Key::ESCAPE, PLAIN, false).is_none());
    }

    #[test]
    fn navigation_works_everywhere() {
        for in_address in [false, true] {
            assert_eq!(name(shortcut(Key::LEFT, ALT, in_address)), "Back");
            assert_eq!(name(shortcut(Key::RIGHT, ALT, in_address)), "Forward");
            assert_eq!(name(shortcut(Key::UP, ALT, in_address)), "Up");
            assert_eq!(name(shortcut(Key::F5, PLAIN, in_address)), "Refresh");
            assert_eq!(name(shortcut(Key::L, CTRL, in_address)), "FocusAddress");
        }
        assert_eq!(name(shortcut(Key::BACK, PLAIN, false)), "Up");
        assert_eq!(name(shortcut(Key::RETURN, ALT, false)), "Properties");
    }

    #[test]
    fn plain_letters_and_arrows_are_not_watched() {
        assert!(!watched(Key::A, PLAIN));
        assert!(!watched(Key::LEFT, PLAIN), "the view moves its focus");
        assert!(!watched(Key::C, PLAIN));
    }
}
