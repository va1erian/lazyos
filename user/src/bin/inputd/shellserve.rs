//! The compositor's shell interface (`os.lazy.input.shell.v1`) served by the
//! hub: `Attach`, surfaces, hotkeys, grants, pointer bounds and the `Note*`
//! one-ways.

use alloc::string::String;
use libmessenger::client::Errno;
use libmessenger::server::{Caller, Fault};
use user::messenger::input::shell_wire;
use user::messenger::Endpoint;

use super::hub::{is_compositor, route_error, Hub, MAX_HOTKEYS};

/// The compositor's calls and notes, on the private channel or the shared
/// endpoint (`Attach`). The `Note*` one-ways answer nothing; a refusal of one
/// is dropped with its reply.
impl shell_wire::Server for Hub {
    fn attach(&mut self, cx: &Caller, events: u64) -> core::result::Result<(), Fault> {
        if !is_compositor(cx.sender) {
            // The message still owns nothing: the decode claimed the endpoint.
            let _ = Endpoint::from_raw(events).release();
            return Err(Fault::errno(Errno::ACCES));
        }
        self.attach_compositor(cx.sender, events);
        Ok(())
    }

    fn set_focus(&mut self, cx: &Caller, surface: Option<u64>) -> core::result::Result<(), Fault> {
        self.require_compositor(cx)?;
        Hub::set_focus(self, surface);
        Ok(())
    }

    fn register_surface(
        &mut self,
        cx: &Caller,
        surface: u64,
        owner: u64,
    ) -> core::result::Result<(), Fault> {
        self.require_compositor(cx)?;
        // Owner 0: the compositor's own surface (its trusted prompt).
        let owner = if owner == 0 { cx.sender } else { owner };
        self.router
            .register_surface(surface, owner)
            .map_err(route_error)
    }

    fn unregister_surface(&mut self, cx: &Caller, surface: u64) -> core::result::Result<(), Fault> {
        self.require_compositor(cx)?;
        if let Some(session) = self.router.unregister_surface(surface) {
            self.forget_endpoint(session);
        }
        Ok(())
    }

    fn register_hotkey(
        &mut self,
        cx: &Caller,
        code: u32,
        mods: u32,
    ) -> core::result::Result<u64, Fault> {
        self.require_compositor(cx)?;
        let code = u16::try_from(code).map_err(|_| Fault::errno(Errno::INVAL))?;
        if self.engine.hotkey_count() >= MAX_HOTKEYS {
            return Err(Fault::errno(Errno::NOMEM));
        }
        // The escape chord is nobody's to register.
        let id = self
            .engine
            .add_hotkey(code, mods)
            .ok_or(Fault::errno(Errno::ACCES))?;
        if let Some(shell) = self.shell.as_mut() {
            shell.hotkeys.push(id);
        }
        Ok(id)
    }

    fn unregister_hotkey(&mut self, cx: &Caller, id: u64) -> core::result::Result<(), Fault> {
        self.require_compositor(cx)?;
        if !self.engine.remove_hotkey(id) {
            return Err(Fault::errno(Errno::NOENT));
        }
        if let Some(shell) = self.shell.as_mut() {
            shell.hotkeys.retain(|held| *held != id);
        }
        Ok(())
    }

    fn approve_grant(
        &mut self,
        cx: &Caller,
        session: u64,
        allow: bool,
    ) -> core::result::Result<(), Fault> {
        self.require_compositor(cx)?;
        self.approve(session, allow)
    }

    fn set_bounds(
        &mut self,
        cx: &Caller,
        width: u32,
        height: u32,
    ) -> core::result::Result<(), Fault> {
        self.require_compositor(cx)?;
        self.set_pointer_bounds(width, height)
    }

    fn get_pointer(
        &mut self,
        cx: &Caller,
    ) -> core::result::Result<shell_wire::GetPointerReply, Fault> {
        self.require_compositor(cx)?;
        Ok(self.pointer_state())
    }

    fn note_focus(&mut self, cx: &Caller, surface: Option<u64>) {
        let _ = shell_wire::Server::set_focus(self, cx, surface);
    }

    fn note_surface(&mut self, cx: &Caller, surface: u64, owner: u64) {
        let _ = shell_wire::Server::register_surface(self, cx, surface, owner);
    }

    fn forget_surface(&mut self, cx: &Caller, surface: u64) {
        let _ = shell_wire::Server::unregister_surface(self, cx, surface);
    }

    fn note_input_done(&mut self, cx: &Caller, seq: u64) {
        if self.require_compositor(cx).is_ok() {
            self.input_done(seq);
        }
    }

    fn note_keys_held(&mut self, cx: &Caller, held: bool) {
        if self.require_compositor(cx).is_ok() {
            self.set_keys_held(held);
        }
    }

    fn note_session_layout(&mut self, cx: &Caller, layout: Option<String>) {
        if self.require_compositor(cx).is_ok() {
            self.set_session_layout(layout.as_deref());
        }
    }

    // These travel inputd to the compositor; one sent to inputd is ignored.
    fn hotkey_fired(&mut self, _: &Caller, _: u64) {}
    fn grant_requested(&mut self, _: &Caller, _: u64, _: u32, _: u64) {}
    fn escape_chord(&mut self, _: &Caller) {}
    fn session_opened(&mut self, _: &Caller, _: u64) {}
    fn session_closed(&mut self, _: &Caller, _: u64) {}
    #[allow(clippy::too_many_arguments)]
    fn pointer_event(
        &mut self,
        _: &Caller,
        _: i32,
        _: i32,
        _: u32,
        _: i32,
        _: i32,
        _: u64,
        _: u64,
    ) {
    }
    fn grab_changed(&mut self, _: &Caller, _: Option<u64>) {}
    fn key_activity(&mut self, _: &Caller) {}
}
