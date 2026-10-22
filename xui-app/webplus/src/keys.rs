//! The window's keyboard shortcuts.

use xui_core::{Key, Modifiers};

use crate::app::Msg;
use crate::bookmarks::BookmarkMsg;
use crate::chrome::Command;
use crate::report::form::ReportMsg;

/// The window's keyboard shortcuts. Enter and Escape mean the address field only while it has
/// the focus, and Escape means Stop only while a page loads: otherwise the page's own forms,
/// dialogs and games get them (a real engine has pages that use Escape and Enter).
pub fn shortcut(key: Key, mods: Modifiers, in_address: bool, loading: bool) -> Option<Msg> {
    let menu = |command| Some(Msg::Menu(command));
    match key {
        Key::RETURN if in_address => Some(Msg::Go),
        Key::ESCAPE if in_address => Some(Msg::RestoreAddress),
        Key::ESCAPE if loading => menu(Command::Stop),
        Key::LEFT if mods.alt => menu(Command::Back),
        Key::RIGHT if mods.alt => menu(Command::Forward),
        Key::HOME if mods.alt => menu(Command::Home),
        Key::F5 => menu(Command::Reload),
        Key::R if mods.ctrl => menu(Command::Reload),
        Key::L if mods.ctrl => Some(Msg::FocusAddress),
        Key::H if mods.ctrl => menu(Command::ShowHistory),
        Key::J if mods.ctrl => menu(Command::ShowDownloads),
        Key::D if mods.ctrl => Some(Msg::Bookmarks(BookmarkMsg::Star)),
        Key::S if mods.ctrl => menu(Command::SavePage),
        Key::W if mods.ctrl => menu(Command::Close),
        _ => None,
    }
}

/// The keys of the frames over the page while one is up: Escape puts the bookmark editor or the
/// report form away, and Enter in the editor's name field is Done. The address field keeps its own
/// Escape and Enter (`shortcut`), so this is asked only when it does not have the focus.
pub fn overlay_shortcut(
    key: Key,
    editor_open: bool,
    report_open: bool,
    in_name_field: bool,
) -> Option<Msg> {
    match key {
        Key::ESCAPE if editor_open => Some(Msg::Bookmarks(BookmarkMsg::Cancel)),
        Key::ESCAPE if report_open => Some(Msg::Report(ReportMsg::Hide)),
        Key::RETURN if editor_open && in_name_field => Some(Msg::Bookmarks(BookmarkMsg::Done)),
        _ => None,
    }
}

/// Whether the page must not see `key`: it is one of the window's own shortcuts. (Enter and
/// Escape in the address field are the field's, never the page's, so they need not be listed.)
pub fn is_window_key(key: Key, mods: Modifiers, loading: bool) -> bool {
    shortcut(key, mods, false, loading).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl() -> Modifiers {
        Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        }
    }

    fn alt() -> Modifiers {
        Modifiers {
            alt: true,
            ..Modifiers::NONE
        }
    }

    fn is(msg: Option<Msg>, command: Command) -> bool {
        matches!(msg, Some(Msg::Menu(c)) if c == command)
    }

    #[test]
    fn the_browsers_chords() {
        let none = Modifiers::NONE;
        assert!(is(shortcut(Key::LEFT, alt(), false, false), Command::Back));
        assert!(is(
            shortcut(Key::RIGHT, alt(), false, false),
            Command::Forward
        ));
        assert!(is(shortcut(Key::HOME, alt(), false, false), Command::Home));
        assert!(is(shortcut(Key::F5, none, false, false), Command::Reload));
        assert!(is(shortcut(Key::R, ctrl(), false, false), Command::Reload));
        assert!(is(
            shortcut(Key::H, ctrl(), false, false),
            Command::ShowHistory
        ));
        assert!(is(
            shortcut(Key::J, ctrl(), false, false),
            Command::ShowDownloads
        ));
        assert!(is(
            shortcut(Key::S, ctrl(), false, false),
            Command::SavePage
        ));
        assert!(is(shortcut(Key::W, ctrl(), false, false), Command::Close));
        assert!(matches!(
            shortcut(Key::L, ctrl(), false, false),
            Some(Msg::FocusAddress)
        ));
        assert!(shortcut(Key::LEFT, none, false, false).is_none());
        assert!(shortcut(Key::H, none, false, false).is_none());
    }

    #[test]
    fn enter_and_escape_belong_to_the_page_unless_they_mean_the_address_or_a_load() {
        let none = Modifiers::NONE;
        assert!(matches!(
            shortcut(Key::RETURN, none, true, false),
            Some(Msg::Go)
        ));
        assert!(shortcut(Key::RETURN, none, false, false).is_none());
        assert!(matches!(
            shortcut(Key::ESCAPE, none, true, true),
            Some(Msg::RestoreAddress)
        ));
        // Escape stops a load that is under way and is the page's otherwise.
        assert!(is(shortcut(Key::ESCAPE, none, false, true), Command::Stop));
        assert!(shortcut(Key::ESCAPE, none, false, false).is_none());
    }

    #[test]
    fn escape_puts_away_the_frame_over_the_page_and_enter_in_the_name_is_done() {
        assert!(matches!(
            overlay_shortcut(Key::ESCAPE, true, false, false),
            Some(Msg::Bookmarks(BookmarkMsg::Cancel))
        ));
        assert!(matches!(
            overlay_shortcut(Key::ESCAPE, false, true, false),
            Some(Msg::Report(ReportMsg::Hide))
        ));
        assert!(matches!(
            overlay_shortcut(Key::RETURN, true, false, true),
            Some(Msg::Bookmarks(BookmarkMsg::Done))
        ));
        // Enter elsewhere in the editor, or with nothing up, is not ours; nor is Escape then.
        assert!(overlay_shortcut(Key::RETURN, true, false, false).is_none());
        assert!(overlay_shortcut(Key::RETURN, false, true, true).is_none());
        assert!(overlay_shortcut(Key::ESCAPE, false, false, false).is_none());
    }

    #[test]
    fn the_page_does_not_see_the_windows_keys() {
        let none = Modifiers::NONE;
        assert!(is_window_key(Key::F5, none, false));
        assert!(is_window_key(Key::L, ctrl(), false));
        assert!(!is_window_key(Key::A, none, false));
        assert!(!is_window_key(Key::ESCAPE, none, false));
        assert!(is_window_key(Key::ESCAPE, none, true));
        assert!(!is_window_key(Key::RETURN, none, false));
    }
}
