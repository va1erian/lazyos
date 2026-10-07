//! The trusted prompt's keyboard (docs/accounts-plan.md U2, issue #625):
//! keys from `inputd`, under the active layout, from every keyboard.
//!
//! While the prompt is up the compositor gives `inputd`'s focus to a surface
//! of its own, [`PROMPT_SURFACE`], registered with owner 0 (the compositor
//! itself, `RegisterSurface` in idl/input.midl), and reads that surface's
//! session: `TextInput` (what a key types
//! under the active layout, French AZERTY or any other, composed) fills the
//! fields, `KeyEvent` drives Tab, Enter, Escape, Backspace and the arrows.
//! A USB keyboard reaches it like a PS/2 one, since both feed `inputd`. No
//! client can read those keys: a session is opened only by its surface's
//! owner (`inputmap::Router::open`), and this surface is the compositor's.
//!
//! The kernel's own key stream (US scancodes, PS/2) stays the fallback for a
//! boot without `inputd`, and is ignored by the prompt while the session is
//! open, so nothing is typed twice.
//!
//! Serial: `XUID:PROMPT:KEYS source=inputd|kernel`.

use user::messenger::display::key;
use user::messenger::input::{mods, KeyInput, KeySession};
use user::messenger::Endpoint;
use user::sys;

use super::compositor::Compositor;

/// The prompt's surface id for `inputd`. Real surfaces count up from 1, so
/// they never reach it.
pub(super) const PROMPT_SURFACE: u64 = u64::MAX - 1;

/// HID usages (keyboard page) the prompt acts on.
mod hid {
    pub const ENTER: u32 = 0x28;
    pub const ESCAPE: u32 = 0x29;
    pub const BACKSPACE: u32 = 0x2A;
    pub const TAB: u32 = 0x2B;
    pub const RIGHT: u32 = 0x4F;
    pub const LEFT: u32 = 0x50;
    pub const KEYPAD_ENTER: u32 = 0x58;
}

impl Compositor {
    /// Take the keyboard from `inputd` for the prompt, if it is reachable.
    pub(super) fn open_prompt_keys(&mut self) {
        match self.try_open_prompt_keys() {
            Ok(()) => sys::write_str("XUID:PROMPT:KEYS source=inputd\n"),
            Err(why) => sys::write_str(&alloc::format!(
                "XUID:PROMPT:KEYS source=kernel reason={why}\n"
            )),
        }
    }

    fn try_open_prompt_keys(&mut self) -> Result<(), alloc::string::String> {
        let Some(link) = self.input.link.as_ref() else {
            return Err("no-inputd".into());
        };
        // Owner 0: the compositor itself (`RegisterSurface`, idl/input.midl).
        link.register_surface(PROMPT_SURFACE, 0)
            .map_err(|error| alloc::format!("register:{:?}", error.errno()))?;
        match KeySession::open(PROMPT_SURFACE) {
            Ok(session) => {
                self.prompt_keys = Some(session);
                Ok(())
            }
            Err(error) => {
                let _ = link.unregister_surface(PROMPT_SURFACE);
                Err(alloc::format!("open:{:?}", error.errno()))
            }
        }
    }

    /// Give the keyboard back (the prompt closed).
    pub(super) fn close_prompt_keys(&mut self) {
        if let Some(session) = self.prompt_keys.take() {
            session.close();
        }
        if let Some(link) = self.input.link.as_ref() {
            let _ = link.unregister_surface(PROMPT_SURFACE);
        }
    }

    /// The prompt session's endpoint, for the main loop to park on.
    pub(super) fn prompt_key_events(&self) -> Option<Endpoint> {
        self.prompt_keys.as_ref().map(KeySession::events_endpoint)
    }

    /// Whether `inputd` feeds the prompt (the kernel stream is then ignored).
    pub(super) fn prompt_keys_from_inputd(&self) -> bool {
        self.prompt_keys.is_some()
    }

    /// Feed the prompt what `inputd` delivered.
    pub(super) fn pump_prompt_keys(&mut self) {
        loop {
            let Some(session) = self.prompt_keys.as_mut() else {
                return;
            };
            match session.poll() {
                Ok(Some(KeyInput::Down { code, mods })) => self.prompt_hid_key(code, mods),
                Ok(Some(KeyInput::Text(text))) => {
                    for c in text.chars().filter(|c| !c.is_control()) {
                        self.prompt_char(c);
                    }
                }
                Ok(None) => return,
                // `inputd` went away: the kernel stream takes over.
                Err(_) => {
                    self.prompt_keys = None;
                    sys::write_str("XUID:PROMPT:KEYS source=kernel\n");
                    return;
                }
            }
        }
    }

    /// A key from `inputd`: the editing and navigation keys (the text comes
    /// separately, as `TextInput`).
    fn prompt_hid_key(&mut self, code: u32, held: u32) {
        let code = match code {
            hid::ENTER | hid::KEYPAD_ENTER => key::ENTER,
            hid::ESCAPE => key::ESCAPE,
            hid::BACKSPACE => key::BACKSPACE,
            hid::TAB => key::TAB,
            hid::LEFT => key::LEFT,
            hid::RIGHT => key::RIGHT,
            _ => return,
        };
        self.mods.shift = held & mods::SHIFT != 0;
        self.prompt_key(code);
    }
}
