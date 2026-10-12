//! The window's `inputd` session beyond plain key events
//! (`docs/input-plan.md`, I3): the key-state page and the keyboard grab.
//!
//! The decisions are `lazysrb2kart::session` (host-tested); this file does the
//! calls. Serial evidence: `SRB2KART:KEYSTATE:PASS` once the page is attached,
//! `SRB2KART:KEYSTATE:RELEASED keys=<n>` when the page released keys the events
//! left held, `SRB2KART:GRAB:ON` / `SRB2KART:GRAB:OFF reason=<GrantReason>`.

use lazysrb2kart::keymap;
use lazysrb2kart::session::GrabAction;
use xui_app::input::{self, Event as InputEvent, KeyState, KeyStatePage};

use crate::window::Window;

/// `WindowState::Maximized` (`idl/display.midl`).
const MAXIMIZED: u32 = 1;

impl Window {
    /// Attach the key-state page to the window's session, if it has one.
    pub(crate) fn attach_key_page(&mut self) {
        let Some(session) = self.window.input else {
            return;
        };
        match session.attach_key_state() {
            Ok(page) => {
                println!("SRB2KART:KEYSTATE:PASS");
                self.key_page = Some(page);
            }
            Err(code) => println!("SRB2KART:KEYSTATE:FAIL errno={code}"),
        }
    }

    /// One event from the `inputd` session.
    pub(crate) fn session_event(&mut self, session: input::Session, event: InputEvent) {
        match event {
            InputEvent::Key {
                code, sym, state, ..
            } => {
                let Some(key) = keymap::from_session(code, sym) else {
                    return;
                };
                match state {
                    KeyState::Down => self.session_keys.press(code, key, &mut self.keys),
                    KeyState::Up => self.session_keys.release(code, &mut self.keys),
                    KeyState::Repeat => {}
                }
            }
            // Focus left: nothing may stay held, or the player runs on.
            InputEvent::Leave => {
                self.session_keys.release_all(&mut self.keys);
                let action = self.grab.focus(false);
                apply_grab(session, action, self);
            }
            InputEvent::Enter(_) => {
                let action = self.grab.focus(true);
                apply_grab(session, action, self);
            }
            InputEvent::Grant { active, reason } => {
                self.grab.granted(active);
                if active {
                    println!("SRB2KART:GRAB:ON");
                } else {
                    println!("SRB2KART:GRAB:OFF reason={reason}");
                }
            }
            InputEvent::Text(_) | InputEvent::Layout(_) => {}
        }
    }

    /// The page is the authority on what is held: release any key it says
    /// is up (an `Up` lost on the way must not keep the player running).
    pub(crate) fn reconcile_keys(&mut self) {
        let Some(snapshot) = self.key_page.as_ref().and_then(KeyStatePage::snapshot) else {
            return;
        };
        if !snapshot.focused {
            return;
        }
        let released = self
            .session_keys
            .reconcile(|code| snapshot.is_down(code), &mut self.keys);
        if released > 0 {
            println!("SRB2KART:KEYSTATE:RELEASED keys={released}");
        }
    }

    /// A `Configure`: maximized windows hold the keyboard grab.
    pub(crate) fn configured(&mut self, state: u32) {
        if let Some(session) = self.window.input {
            let action = self.grab.configured(state == MAXIMIZED);
            apply_grab(session, action, self);
        }
    }
}

/// Ask for or give back the keyboard grab, as the policy decided.
fn apply_grab(session: input::Session, action: GrabAction, window: &mut Window) {
    match action {
        GrabAction::Request => {
            if let Err(code) = session.request_grant() {
                println!("SRB2KART:GRAB:FAIL errno={code}");
                window.grab.request_failed();
            }
        }
        GrabAction::Release => {
            let _ = session.release_grant();
        }
        GrabAction::None => {}
    }
}
