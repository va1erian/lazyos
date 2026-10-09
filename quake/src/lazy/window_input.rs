//! What the window's pumps produce, and the session plumbing beyond plain
//! keys (`docs/input-plan.md`, I3): the key-state page's releases, and the
//! keyboard grab while maximized. The house-tested decisions are
//! [`session`]; this file does the calls. Serial evidence:
//! `QUAKE:KEYSTATE:PASS` once the page is attached, `QUAKE:GRAB:ON` /
//! `QUAKE:GRAB:OFF reason=<comment>` and the same `FAIL errno=` shape the
//! Doom port prints.

use xui_app::input::{self, Event as InputEvent, KeyState, KeyStatePage};

use crate::lazy::keymap;
use crate::lazy::session::GrabAction;

use super::window::{KeyRecord, Window};

/// `WindowState::Maximized` (`idl/display.midl`).
const MAXIMIZED: u32 = 1;

/// One thing the window's pump produced: a key edge, or the batched
/// `ClearAllStates` the engine applies in one go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpEvent {
    Key(KeyRecord),
    ClearKeys,
}

impl Window {
    /// Attach the key-state page to the window's session, if it has one.
    pub(crate) fn attach_key_page(&mut self) {
        let Some(session) = self.window.input else {
            return;
        };
        match session.attach_key_state() {
            Ok(page) => {
                println!("QUAKE:KEYSTATE:PASS");
                self.key_page = Some(page);
            }
            Err(code) => println!("QUAKE:KEYSTATE:FAIL errno={code}"),
        }
    }

    /// One event from the `inputd` session.
    pub(crate) fn session_event(
        &mut self,
        session: input::Session,
        event: InputEvent,
        out: &mut Vec<PumpEvent>,
    ) {
        match event {
            InputEvent::Key {
                code, sym, state, ..
            } => {
                let Some(edge) = keymap::from_session(code, sym) else {
                    return;
                };
                match state {
                    KeyState::Down => {
                        if self.session_keys.press(code, edge) {
                            out.push(PumpEvent::Key(KeyRecord {
                                keynum: edge.0,
                                down: true,
                                ch: edge.1,
                            }));
                        }
                    }
                    KeyState::Up => {
                        if let Some(edge) = self.session_keys.release(code) {
                            out.push(PumpEvent::Key(KeyRecord {
                                keynum: edge.0,
                                down: false,
                                ch: edge.1,
                            }));
                        }
                    }
                    KeyState::Repeat => {}
                }
            }
            // Focus left: nothing may stay held, or the player runs on and
            // the menu answers a stale y. Every held edge is released and
            // `ClearAllStates` sent, as `keys.c` does.
            InputEvent::Leave => {
                for edge in self.session_keys.release_all() {
                    out.push(PumpEvent::Key(KeyRecord {
                        keynum: edge.0,
                        down: false,
                        ch: edge.1,
                    }));
                }
                out.push(PumpEvent::ClearKeys);
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
                    println!("QUAKE:GRAB:ON");
                } else {
                    println!("QUAKE:GRAB:OFF reason={reason}");
                }
            }
            InputEvent::Text(_) | InputEvent::Layout(_) => {}
        }
    }

    /// The page is the authority on what is held: release any key it says
    /// is up (an `Up` lost on the way must not keep the player running).
    pub(crate) fn reconcile_keys(&mut self, out: &mut Vec<PumpEvent>) {
        let Some(snapshot) = self.key_page.as_ref().and_then(KeyStatePage::snapshot) else {
            return;
        };
        if !snapshot.focused {
            return;
        }
        for edge in self.session_keys.reconcile(|code| snapshot.is_down(code)) {
            out.push(PumpEvent::Key(KeyRecord { keynum: edge.0, down: false, ch: edge.1 }));
        }
    }

    /// A `Configure`: maximized windows hold the keyboard grab.
    pub(crate) fn maximized_configured(&mut self, state: u32) {
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
                println!("QUAKE:GRAB:FAIL errno={code}");
                window.grab.request_failed();
            }
        }
        GrabAction::Release => {
            let _ = session.release_grant();
        }
        GrabAction::None => {}
    }
}
