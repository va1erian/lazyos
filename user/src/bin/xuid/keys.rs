//! Keyboard routing: modifier tracking, the compositor's global hotkeys
//! (Alt+Tab, Tab focus cycling, Alt+F4, Ctrl+Esc, Escape) and forwarding of
//! everything else to the focused surface.

use user::messenger::display::{self, wire};

use super::compositor::Compositor;
use super::shell::modifier_key;
use super::window::{cycle_focus, forward};

impl Compositor {
    /// A key went down.
    pub(super) fn key_down(&mut self, key: u32) {
        // Modifier keys are compositor-level (issue #167): track them and
        // never forward them to a client.
        if modifier_key(key) {
            match key {
                display::key::SHIFT => self.mods.shift = true,
                display::key::CTRL => self.mods.ctrl = true,
                display::key::ALT => self.mods.alt = true,
                display::key::SUPER => {
                    self.mods.super_key = true;
                    self.notify_start_menu();
                }
                _ => {}
            }
            return;
        }
        if key == display::key::ESCAPE && self.escape_pressed() {
            return;
        }
        // Tab belongs to the focused client (widget focus, tab characters);
        // the compositor only takes it as Alt+Tab, as Ctrl+Tab, or when no
        // window has focus to give it to.
        if key == display::key::TAB && (self.mods.alt || self.mods.ctrl || self.focused.is_none()) {
            if self.mods.alt {
                // Alt+Tab: the compositor's own overlay, not a client key.
                self.alt_tab_open();
            } else {
                let before = self.focused;
                cycle_focus(&mut self.surfaces, &mut self.focused);
                if self.focused != before {
                    self.notify_focus();
                }
                self.repaint_full();
            }
            return;
        }
        if key == display::key::F4 && self.mods.alt {
            // Alt+F4: ask the focused window to close, exactly like its X
            // button.
            if let Some(id) = self.focused {
                self.close_surface(id);
            }
            return;
        }
        let key = self.client_key(key);
        let body = wire::encode_key_down_args(&wire::KeyDownArgs { key });
        forward(
            &self.surfaces,
            &mut self.scratch,
            self.focused,
            wire::METHOD_KEYDOWN,
            body,
        );
    }

    /// Escape: close the menu, then the Alt+Tab overlay, then act as the
    /// Ctrl+Esc start-menu chord, then cancel a drag & drop. Returns whether
    /// the compositor consumed the key.
    fn escape_pressed(&mut self) -> bool {
        if self.menu_escape() {
            return true;
        }
        // Escape closes the Alt+Tab overlay first...
        if self.alt_tab.take().is_some() {
            self.repaint_full();
            return true;
        }
        // ...then it is the Ctrl+Esc start-menu chord...
        if self.mods.ctrl {
            self.notify_start_menu();
            return true;
        }
        // ...and otherwise it cancels a live drag & drop (issue #145).
        if self.drag_session.is_some() {
            self.drag_cancel();
            return true;
        }
        false
    }

    /// `key` with the held modifiers OR-ed in, as clients receive it.
    fn client_key(&self, key: u32) -> u32 {
        let mods = &self.mods;
        display::key::with_modifiers(key, mods.shift, mods.ctrl, mods.alt, mods.super_key)
    }

    /// A key went up.
    pub(super) fn key_up(&mut self, key: u32) {
        if modifier_key(key) {
            match key {
                display::key::SHIFT => self.mods.shift = false,
                display::key::CTRL => self.mods.ctrl = false,
                display::key::ALT => {
                    self.mods.alt = false;
                    // Releasing Alt commits the Alt+Tab selection.
                    if let Some(tab) = self.alt_tab.take() {
                        self.alt_tab_commit(&tab);
                    }
                }
                display::key::SUPER => self.mods.super_key = false,
                _ => {}
            }
            return;
        }
        // The release half of the Ctrl+Esc chord is consumed as well.
        if key == display::key::ESCAPE && self.mods.ctrl {
            return;
        }
        let key = self.client_key(key);
        let body = wire::encode_key_up_args(&wire::KeyUpArgs { key });
        forward(
            &self.surfaces,
            &mut self.scratch,
            self.focused,
            wire::METHOD_KEYUP,
            body,
        );
    }
}
