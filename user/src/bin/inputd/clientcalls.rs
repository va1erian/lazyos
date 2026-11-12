//! `os.lazy.input.v1` requests: open and close sessions, state, grabs, the
//! key-state and gamepad-state pages and `Ping`. Key content itself is pushed (`hub.rs`).

use alloc::string::String;
use alloc::vec::Vec;
use libmessenger::client::Errno;
use libmessenger::server::{Caller, Fault};
use user::sys;
use user::trace;

use inputmap::{REPEAT_DELAY_TICKS, REPEAT_INTERVAL_TICKS};
use user::messenger::input::{shell_wire, wire};
use user::messenger::Endpoint;

use super::hub::{route_error, Hub};

/// PIT ticks are 10 ms.
const TICK_MS: u32 = 10;

impl wire::Server for Hub {
    /// `Open`: bind a session to a surface the sender owns and adopt the
    /// event endpoint it carried; every way out but success closes it.
    fn open(&mut self, cx: &Caller, surface: Option<u64>, events: u64) -> Result<u64, Fault> {
        let opened = self.open_session(cx.sender, surface, events);
        if opened.is_err() {
            // Decoded, so the endpoint is ours to close.
            let _ = Endpoint::from_raw(events).release();
        }
        opened
    }

    /// `Close`: only the session's owner may.
    fn close(&mut self, cx: &Caller, session: u64) -> Result<(), Fault> {
        let surface = self.router.close(session, cx.sender).map_err(route_error)?;
        self.forget_endpoint(session);
        if let Some(surface) = surface {
            self.announce_closed(surface);
        }
        Ok(())
    }

    fn get_state(&mut self, _: &Caller) -> Result<wire::GetStateReply, Fault> {
        Ok(wire::GetStateReply {
            layout: self.engine.layout().name().into(),
            mods: self.engine.mods(),
            repeat_delay_ms: REPEAT_DELAY_TICKS as u32 * TICK_MS,
            repeat_interval_ms: REPEAT_INTERVAL_TICKS as u32 * TICK_MS,
        })
    }

    fn request_grant(&mut self, cx: &Caller, session: u64, kind: u32) -> Result<(), Fault> {
        self.request_keyboard_grant(session, kind, cx.sender)
    }

    fn release_grant(&mut self, cx: &Caller, session: u64) -> Result<(), Fault> {
        self.own_session(session, cx.sender)?;
        if let Some(change) = self.grabs.release(session) {
            self.grant_change(change);
        }
        Ok(())
    }

    fn ping(&mut self, cx: &Caller, session: u64, token: u64) -> Result<wire::PingReply, Fault> {
        let found = self
            .router
            .session(session)
            .ok_or_else(|| Fault::errno(Errno::NOENT))?;
        // The live sequence number reveals when keys are typed: only the
        // session they are typed into may read it.
        if found.owner != cx.sender {
            return Err(Fault::errno(Errno::ACCES));
        }
        let seq = if self.router.focused_session() == Some(session) {
            self.engine.last_seq()
        } else {
            0
        };
        Ok(wire::PingReply { token, seq })
    }

    /// `AttachKeyState`: adopt the page the caller's own session carried; a
    /// refusal closes it (it was decoded, so it is ours).
    fn attach_key_state(
        &mut self,
        cx: &Caller,
        session: u64,
        state: libmessenger::Buffer,
    ) -> Result<(), Fault> {
        let attached = self.adopt_key_state(cx.sender, session, &state);
        if attached.is_err() {
            let _ = sys::buffer_close(state.handle);
        }
        attached
    }

    /// `AttachGamepadState`, the same for the pads' page.
    fn attach_gamepad_state(
        &mut self,
        cx: &Caller,
        session: u64,
        state: libmessenger::Buffer,
    ) -> Result<(), Fault> {
        let attached = self.adopt_gamepad_state(cx.sender, session, &state);
        if attached.is_err() {
            let _ = sys::buffer_close(state.handle);
        }
        attached
    }

    // Events travel inputd to its clients; one sent to inputd is ignored.
    fn key_event(&mut self, _: &Caller, _: u32, _: u32, _: u32, _: u32, _: u64, _: u64) {}
    fn text_input(&mut self, _: &Caller, _: String) {}
    fn keyboard_enter(&mut self, _: &Caller, _: Vec<u32>) {}
    fn keyboard_leave(&mut self, _: &Caller) {}
    fn layout_changed(&mut self, _: &Caller, _: String) {}
    fn grant_changed(&mut self, _: &Caller, _: u32, _: bool, _: u32) {}
    fn gamepad_axis(&mut self, _: &Caller, _: u32, _: u32, _: i32, _: u64, _: u64) {}
    fn gamepad_button(&mut self, _: &Caller, _: u32, _: u32, _: bool, _: u64, _: u64) {}
    fn gamepad_enter(&mut self, _: &Caller) {}
    fn gamepad_leave(&mut self, _: &Caller) {}
}

impl Hub {
    fn open_session(
        &mut self,
        sender: u64,
        surface: Option<u64>,
        events: u64,
    ) -> Result<u64, Fault> {
        // A session without a surface is the login console's (issue #396).
        let Some(surface) = surface else {
            return self.open_console(sender, events);
        };
        let opened = self.router.open(sender, surface).map_err(route_error)?;
        if let Some(old) = opened.replaced {
            self.forget_endpoint(old);
        }
        self.delivery
            .insert(opened.session, Endpoint::from_raw(events));
        // Sessions are rare (one per window), so each is worth a boot-log line.
        trace::line!(
            trace::Channel::Inputd,
            "SESSION:OPEN session={} surface={surface} owner={sender}",
            opened.session,
        );
        if opened.first_for_surface {
            self.shell_event(
                shell_wire::METHOD_SESSIONOPENED,
                shell_wire::encode_session_opened_args(&shell_wire::SessionOpenedArgs { surface }),
            );
        }
        if opened.focused {
            self.enter(opened.session);
        }
        Ok(opened.session)
    }
}
