//! The login console's sessionless input session (issue #396).
//!
//! `Open` with no surface opens the console session. Only the task holding
//! the kernel's console claim may (syscall 25 op 7, which needs
//! `CAP_INPUT_CONSOLE`, stamped onto `logind` alone; the kernel says who
//! holds it), so no client can open one to read keys meant for the login
//! prompt. The session takes the keyboard while no compositor is attached
//! (`inputmap::Router`): exactly when the console is on screen, and when the
//! kernel keeps typed keys off its terminal queue for the claimant.
//!
//! Serial: `INPUTD:CONSOLE:OPEN session=<id> owner=<slot> focused=<0|1>`,
//! `INPUTD:CONSOLE:DENY owner=<slot>`.

use alloc::format;
use alloc::vec::Vec;

use user::messenger::input::wire;
use user::messenger::{errno, Endpoint, Error, Message, Result};
use user::sys;

use super::hub::{route_error, Hub};

impl Hub {
    /// `Open(None)`: the console session, for the console claim's holder;
    /// `events` is the endpoint the request carried.
    pub(super) fn open_console(&mut self, message: &Message, events: u64) -> Result<Vec<u8>> {
        if sys::input_console_owner() != Ok(message.sender) {
            sys::write_str(&format!("INPUTD:CONSOLE:DENY owner={}\n", message.sender));
            return Err(Error::Errno(-errno::EACCES));
        }
        let opened = self
            .router
            .open_console(message.sender)
            .map_err(route_error)?;
        if let Some(old) = opened.replaced {
            self.forget_endpoint(old);
        }
        self.delivery
            .insert(opened.session, Endpoint::from_raw(events));
        sys::write_str(&format!(
            "INPUTD:CONSOLE:OPEN session={} owner={} focused={}\n",
            opened.session,
            message.sender,
            u8::from(opened.focused)
        ));
        if opened.focused {
            self.enter(opened.session);
        }
        wire::encode_open_reply(&wire::OpenReply {
            session: opened.session,
        })
        .map_err(Error::Parcel)
    }
}
