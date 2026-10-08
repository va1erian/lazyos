//! The shell's panel keys (issue #648): a `Panel` never takes keyboard focus,
//! so while one of LazyShell's panel menus is open (the start menu, a
//! submenu, a tray menu, the tray reached with Win+B) the shell asks for the
//! keyboard with `GrabPanelKeys(true)`, and every key the compositor does not
//! keep for itself goes to the shell as a `PanelKey` event instead of to the
//! focused window. The window keeps its focus (its title stays lit), and
//! `inputd` is told no window has the keyboard, so nothing reaches it twice.
//!
//! The grab is the shell's alone: the events go to the shell subscriber,
//! never to a privileged observer, which must not read keystrokes. It ends
//! with `GrabPanelKeys(false)` or when the shell changes or dies, and it
//! stands aside for a client's keyboard grab and the trusted prompt.
//!
//! Super+B sends `TrayKeys`; Super pressed and released alone sends
//! `StartMenu` (on the release, so the chord can use Super first).
//!
//! Serial: `XUID:PANELKEYS:GRAB on|off`.

use alloc::vec::Vec;
use libmessenger::Parcel;
use user::messenger::display::wire;
use user::messenger::{self, Endpoint, Message};
use user::sys;

use super::compositor::Compositor;
use super::protocol::{empty_reply, error_reply};

/// The key code of `b`, as the kernel reports letters (lowercase).
pub(super) const TRAY_KEY: u32 = b'b' as u32;

impl Compositor {
    /// `GrabPanelKeys`: only the shell's own task may take the keyboard.
    pub(super) fn grab_panel_keys(&mut self, message: &Message, body: &[u8]) -> Parcel {
        let is_shell = self
            .shell
            .as_ref()
            .is_some_and(|shell| shell.task == message.sender && !shell.dead);
        if !is_shell {
            return error_reply(message.method(), messenger::errno::EACCES);
        }
        let Ok(args) = wire::decode_grab_panel_keys_args(body) else {
            return error_reply(message.method(), messenger::errno::EINVAL);
        };
        self.set_panel_keys(args.grab);
        empty_reply(message.method())
    }

    /// Start or end the shell's grab, telling `inputd` at once who has the
    /// keyboard now.
    pub(super) fn set_panel_keys(&mut self, grab: bool) {
        if self.panel_keys == grab {
            return;
        }
        self.panel_keys = grab;
        sys::write_str(if grab {
            "XUID:PANELKEYS:GRAB on\n"
        } else {
            "XUID:PANELKEYS:GRAB off\n"
        });
        self.push_input();
    }

    /// Whether keys go to the shell's panels now: the shell holds the grab,
    /// and neither a client's keyboard grab nor the prompt overrides it.
    pub(super) fn panel_keys_active(&self) -> bool {
        self.panel_keys
            && self.shell.as_ref().is_some_and(|shell| !shell.dead)
            && self.input.grab.is_none()
            && self.prompt.is_none()
    }

    /// Send `key` (modifiers OR-ed in) to the shell as `PanelKey`.
    pub(super) fn send_panel_key(&mut self, key: u32) {
        let body = wire::encode_panel_key_args(&wire::PanelKeyArgs { key });
        self.send_to_shell(wire::METHOD_PANELKEY, body);
    }

    /// Super+B: move the keyboard to the tray.
    pub(super) fn notify_tray_keys(&mut self) {
        self.send_to_shell(wire::METHOD_TRAYKEYS, Ok(Vec::new()));
    }

    /// One event to the shell alone (no observer copy: these are keys).
    fn send_to_shell(&mut self, method: u32, body: Result<Vec<u8>, libmessenger::Error>) {
        let Some(shell) = self.shell.as_mut() else {
            return;
        };
        let result = user::messenger::display::send_event(
            &Endpoint::from_raw(shell.events),
            &mut self.scratch,
            method,
            body,
        );
        if matches!(result, Err(messenger::Error::Errno(code)) if code == -messenger::errno::EPIPE)
        {
            shell.dead = true;
        }
    }
}
