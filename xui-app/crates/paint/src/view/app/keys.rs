#![forbid(unsafe_code)]

//! The window's key hook: Paint.NET's shortcuts ahead of the focused widget,
//! kept out of the way of dialogs, menus and text or number fields.

use std::cell::Cell;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::message::Key;
use xui_core::widget::Menu;

use crate::view::shortcuts::{float_key, is_plain, shortcut, typing_shortcut};
use crate::view::{KeyOwner, Msg};

/// What the hook reads; the app rewrites it after every message.
pub(super) struct Flags {
    /// Whether a dialog, the editor or a menu has the keyboard.
    pub modal: Rc<Cell<bool>>,
    /// Whether the arrow keys, Enter and Esc belong to a move tool or a float.
    pub float_keys: Rc<Cell<bool>>,
    /// Whether a text block is being edited, so letters are text, not tools.
    pub typing: Rc<Cell<bool>>,
    /// The OK and Cancel messages of the open frame.
    pub frame_keys: Rc<Cell<Option<(Msg, Msg)>>>,
}

/// Installs the hook on `ui`. `owners` are the controls that leave plain keys
/// alone: with the keyboard anywhere else only commands are claimed.
pub(super) fn install(ui: &Ui<Msg>, menu: &Rc<Menu<Msg>>, owners: [KeyOwner; 4], flags: Flags) {
    // Weak: the menu holds the window, and the window holds this hook.
    let menu = Rc::downgrade(menu);
    ui.on_key(move |key, modifiers| {
        let menu_open = menu.upgrade().is_some_and(|menu| menu.is_open());
        let blocked = flags.modal.get() || menu_open;
        // An Image or Adjustments dialog is a frame, not a card, so Enter and
        // Esc are routed to it here, and nothing else (no tool letter) gets
        // past it: `modal` blocks the table. Enter waits behind whatever
        // button has the keyboard (see `Msg::FrameEnter`).
        if let Some((_, cancel)) = flags.frame_keys.get().filter(|_| !menu_open) {
            match key {
                Key::RETURN => return Some(Msg::FrameEnter),
                Key::ESCAPE => return Some(cancel),
                _ => {}
            }
        }
        if is_plain(key, modifiers) && !owners.iter().any(KeyOwner::get) {
            return None;
        }
        let chosen = if flags.typing.get() {
            typing_shortcut(key, modifiers, blocked)
        } else {
            shortcut(key, modifiers, blocked)
        };
        chosen.or_else(|| {
            (flags.float_keys.get() && !blocked)
                .then(|| float_key(key, modifiers))
                .flatten()
        })
    });
}
