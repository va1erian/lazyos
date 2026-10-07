//! Keyboard grabs, compositor side (`docs/input-plan.md`, I3).
//!
//! A client asks `inputd` for a keyboard grab; `inputd` asks us
//! (`GrantRequested`) and we approve it only for the window that has keyboard
//! focus right now and is on screen: a background or minimized window can
//! never take the keyboard. While a grab is held (`GrabChanged`), `keys.rs`
//! leaves every chord to the grabbing window. The user always gets out with
//! the reserved escape chord, Ctrl+Alt+Esc, which `inputd` handles before any
//! grab and which nobody can register; focusing another window with the
//! mouse ends the grab too.
//!
//! Serial: `XUID:GRAB:ASK surface=<id> allow=<0|1>`,
//! `XUID:GRAB:HELD surface=<id>|none`, `XUID:GRAB:ESCAPE`.

use alloc::format;

use user::messenger::input::{wire, ShellEvent};
use user::sys;

use super::compositor::Compositor;

impl Compositor {
    /// A grant-related shell event from `inputd`.
    pub(super) fn grant_event(&mut self, event: ShellEvent) {
        match event {
            ShellEvent::GrantRequested {
                session,
                kind,
                surface,
            } => self.answer_grant(session, kind, surface),
            ShellEvent::GrabChanged(surface) => {
                self.input.grab = surface;
                match surface {
                    Some(id) => sys::write_str(&format!("XUID:GRAB:HELD surface={id}\n")),
                    None => sys::write_str("XUID:GRAB:HELD surface=none\n"),
                }
            }
            ShellEvent::EscapeChord => sys::write_str("XUID:GRAB:ESCAPE\n"),
            // The compositor keeps its own hotkey table (`keys.rs`).
            _ => {}
        }
    }

    /// Approve a keyboard grab for the focused, visible window only.
    fn answer_grant(&mut self, session: u64, kind: u32, surface: u64) {
        let on_screen = self
            .surfaces
            .iter()
            .any(|s| s.id == surface && s.is_window() && !s.minimized);
        // Never while the trusted prompt has the keyboard (`prompt.rs`).
        let allow = kind == wire::GRANT_KIND_KEYBOARD
            && self.focused == Some(surface)
            && on_screen
            && self.prompt.is_none();
        sys::write_str(&format!(
            "XUID:GRAB:ASK surface={surface} allow={}\n",
            u8::from(allow)
        ));
        if let Some(link) = self.input.link.as_ref() {
            // A refusal (the request was withdrawn meanwhile) changes nothing.
            let _ = link.approve_grant(session, allow);
        }
    }
}
