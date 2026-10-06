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

use alloc::format;
use alloc::vec::Vec;

use inputmap::grab::{Change, Reason, Refused, Requested};
use user::messenger::input::{shell_wire, wire};
use user::messenger::{errno, Error, Message, Result};
use user::sys;

use super::hub::Hub;

impl Hub {
    /// `RequestGrant`: ask the compositor on behalf of the caller's own,
    /// focused session.
    pub(super) fn request_grant(&mut self, message: &Message) -> Result<Vec<u8>> {
        let args = wire::decode_request_grant_args(&message.parcel.body).map_err(Error::Parcel)?;
        self.own_session(args.session, message.sender)?;
        if args.kind != wire::GRANT_KIND_KEYBOARD {
            return Err(Error::Errno(-errno::EINVAL));
        }
        let focused = self.router.focused_session();
        match self.grabs.request(args.session, focused) {
            Err(Refused::NotFocused) => Err(Error::Errno(-errno::EACCES)),
            Ok(Requested::AlreadyHeld) => Ok(Vec::new()),
            Ok(Requested::Pending { withdrawn }) => {
                if let Some(change) = withdrawn {
                    self.grant_changed(change);
                }
                let surface = self
                    .router
                    .session(args.session)
                    .and_then(|s| s.surface)
                    .unwrap_or(0);
                sys::write_str(&format!(
                    "INPUTD:GRAB:REQUEST session={} surface={surface}\n",
                    args.session
                ));
                if !self.shell_attached() {
                    // Nobody to approve it: a grab is never self-granted.
                    if let Some(change) = self.grabs.approve(args.session, false, focused) {
                        self.grant_changed(change);
                    }
                    return Ok(Vec::new());
                }
                self.shell_event(
                    shell_wire::METHOD_GRANTREQUESTED,
                    shell_wire::encode_grant_requested_args(&shell_wire::GrantRequestedArgs {
                        session: args.session,
                        kind: args.kind,
                        surface,
                    }),
                );
                Ok(Vec::new())
            }
        }
    }

    /// `ReleaseGrant`: the caller's session gives up its grab or request.
    pub(super) fn release_grant(&mut self, message: &Message) -> Result<Vec<u8>> {
        let args = wire::decode_release_grant_args(&message.parcel.body).map_err(Error::Parcel)?;
        self.own_session(args.session, message.sender)?;
        if let Some(change) = self.grabs.release(args.session) {
            self.grant_changed(change);
        }
        Ok(Vec::new())
    }

    /// `ApproveGrant`, from the attached compositor (the caller checked).
    pub(super) fn approve_grant(&mut self, body: &[u8]) -> Result<Vec<u8>> {
        let args = shell_wire::decode_approve_grant_args(body).map_err(Error::Parcel)?;
        let focused = self.router.focused_session();
        let change = self
            .grabs
            .approve(args.session, args.allow, focused)
            .ok_or(Error::Errno(-errno::ENOENT))?;
        self.grant_changed(change);
        Ok(Vec::new())
    }

    /// The reserved escape chord: revert any grab, tell the compositor.
    pub(super) fn escape_chord(&mut self) {
        sys::write_str("INPUTD:GRAB:ESCAPE\n");
        if let Some(change) = self.grabs.escape() {
            self.grant_changed(change);
        }
        self.shell_event(shell_wire::METHOD_ESCAPECHORD, Ok(Vec::new()));
    }

    /// Focus moved: a grab or request that lost it ends.
    pub(super) fn grants_follow_focus(&mut self) {
        let focused = self.router.focused_session();
        if let Some(change) = self.grabs.focus_changed(focused) {
            self.grant_changed(change);
        }
    }

    /// `session` is gone: its grab or request ends (nobody to tell but the
    /// compositor).
    pub(super) fn grants_forget(&mut self, session: u64) {
        if let Some(change) = self.grabs.closed(session) {
            self.grant_changed(change);
        }
    }

    /// Apply one change: the engine's hotkey bypass, the client's event, the
    /// compositor's view of the holder.
    fn grant_changed(&mut self, change: Change) {
        let holder = self.grabs.holder();
        self.engine.set_grabbed(holder.is_some());
        if change.active {
            let surface = self.surface_of(change.session);
            sys::write_str(&format!(
                "INPUTD:GRAB:ON session={} surface={}\n",
                change.session,
                surface.unwrap_or(0)
            ));
        } else {
            sys::write_str(&format!(
                "INPUTD:GRAB:OFF session={} reason={}\n",
                change.session, change.reason as u32
            ));
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
