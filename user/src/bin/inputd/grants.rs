//! Keyboard grabs (I3): `RequestGrant`/`ReleaseGrant` from clients,
//! `ApproveGrant` from the compositor, and the reserved escape chord.
//!
//! The decisions are `inputmap::Grabs` (host-tested); this module turns each
//! change into Messenger traffic: `GrantChanged` to the client, `GrabChanged`
//! to the compositor (which then stops acting on its own chords from the
//! kernel key stream), and serial lines
//! `INPUTD:GRAB:ON session=<id> surface=<id>`,
//! `INPUTD:GRAB:OFF session=<id> reason=<n>`, `INPUTD:GRAB:REQUEST ...`,
//! `INPUTD:GRAB:ESCAPE`.

use alloc::vec::Vec;
use user::trace;

use inputmap::grab::{Change, Reason, Refused, Requested};
use libmessenger::client::Errno;
use libmessenger::server::Fault;
use user::messenger::input::{shell_wire, wire};

use super::hub::Hub;

impl Hub {
    /// `RequestGrant`: ask the compositor on behalf of the caller's own,
    /// focused session.
    pub(super) fn request_keyboard_grant(
        &mut self,
        session: u64,
        kind: u32,
        sender: u64,
    ) -> Result<(), Fault> {
        self.own_session(session, sender)?;
        if kind != wire::GRANT_KIND_KEYBOARD {
            return Err(Fault::errno(Errno::INVAL));
        }
        let focused = self.router.focused_session();
        match self.grabs.request(session, focused) {
            Err(Refused::NotFocused) => Err(Fault::errno(Errno::ACCES)),
            Ok(Requested::AlreadyHeld) => Ok(()),
            Ok(Requested::Pending { withdrawn }) => {
                if let Some(change) = withdrawn {
                    self.grant_change(change);
                }
                let surface = self
                    .router
                    .session(session)
                    .and_then(|s| s.surface)
                    .unwrap_or(0);
                trace::line!(
                    trace::Channel::Inputd,
                    "GRAB:REQUEST session={} surface={surface}",
                    session
                );
                if !self.shell_attached() {
                    // Nobody to approve it: a grab is never self-granted.
                    if let Some(change) = self.grabs.approve(session, false, focused) {
                        self.grant_change(change);
                    }
                    return Ok(());
                }
                self.shell_event(
                    shell_wire::METHOD_GRANTREQUESTED,
                    shell_wire::encode_grant_requested_args(&shell_wire::GrantRequestedArgs {
                        session,
                        kind,
                        surface,
                    }),
                );
                Ok(())
            }
        }
    }

    /// `ApproveGrant`, from the attached compositor (the caller checked).
    pub(super) fn approve(&mut self, session: u64, allow: bool) -> Result<(), Fault> {
        let focused = self.router.focused_session();
        let change = self
            .grabs
            .approve(session, allow, focused)
            .ok_or_else(|| Fault::errno(Errno::NOENT))?;
        self.grant_change(change);
        Ok(())
    }

    /// The reserved escape chord: revert any grab, tell the compositor.
    pub(super) fn escape_chord(&mut self) {
        trace::line!(trace::Channel::Inputd, "GRAB:ESCAPE");
        if let Some(change) = self.grabs.escape() {
            self.grant_change(change);
        }
        self.shell_event(shell_wire::METHOD_ESCAPECHORD, Ok(Vec::new()));
    }

    /// Focus moved: a grab or request that lost it ends.
    pub(super) fn grants_follow_focus(&mut self) {
        let focused = self.router.focused_session();
        if let Some(change) = self.grabs.focus_changed(focused) {
            self.grant_change(change);
        }
    }

    /// `session` is gone: its grab or request ends (nobody to tell but the
    /// compositor).
    pub(super) fn grants_forget(&mut self, session: u64) {
        if let Some(change) = self.grabs.closed(session) {
            self.grant_change(change);
        }
    }

    /// Apply one change: the engine's hotkey bypass, the client's event, the
    /// compositor's view of the holder.
    pub(super) fn grant_change(&mut self, change: Change) {
        let holder = self.grabs.holder();
        self.engine.set_grabbed(holder.is_some());
        if change.active {
            let surface = self.surface_of(change.session);
            trace::line!(
                trace::Channel::Inputd,
                "GRAB:ON session={} surface={}",
                change.session,
                surface.unwrap_or(0)
            );
        } else {
            trace::line!(
                trace::Channel::Inputd,
                "GRAB:OFF session={} reason={}",
                change.session,
                change.reason as u32
            );
        }
        if change.reason != Reason::Closed {
            let body = wire::encode_grant_changed_args(&wire::GrantChangedArgs {
                kind: wire::GRANT_KIND_KEYBOARD,
                active: change.active,
                reason: change.reason as u32,
            });
            self.send(change.session, wire::METHOD_GRANTCHANGED, body);
        }
        if change.holder_changed {
            let surface = holder.and_then(|session| self.surface_of(session));
            self.shell_event(
                shell_wire::METHOD_GRABCHANGED,
                shell_wire::encode_grab_changed_args(&shell_wire::GrabChangedArgs { surface }),
            );
        }
    }

    fn surface_of(&self, session: u64) -> Option<u64> {
        self.router.session(session).and_then(|s| s.surface)
    }
}
