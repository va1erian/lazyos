//! `SetCloseMode` (method 48): an app that must ask before it closes keeps its
//! window when the window manager asks it to (`WindowClose`). Only the creator
//! may set it, and it only applies to windows (not the desktop or panels).
//!
//! An app must never make its window unclosable: the first close is passed on
//! to the app, which has [`GRACE_NS`] to answer (by destroying the surface or
//! by calling `SetCloseMode` again, which re-arms the question); a close after
//! that forgets the surface whatever the app says, so a hung or hostile client
//! cannot keep a window. A shutdown or logout never goes through here: `init`
//! kills what is left, and the compositor drops a dead owner's surfaces.

use libmessenger::Parcel;
use user::messenger::display::wire;
use user::messenger::{self, Message};

use super::compositor::Compositor;
use super::protocol::{empty_reply, error_reply};
use super::window::surface_by_id;

impl Compositor {
    /// `SetCloseMode`: keep (`ask`) or drop (default) the window when it is
    /// closed.
    pub(super) fn set_close_mode(&mut self, message: &Message, body: &[u8]) -> Parcel {
        let Ok(args) = wire::decode_set_close_mode_args(body) else {
            return error_reply(message.method(), messenger::errno::EINVAL);
        };
        let Some(surface) = surface_by_id(&self.surfaces, args.surface) else {
            return error_reply(message.method(), messenger::errno::ENOENT);
        };
        if surface.owner != message.sender {
            return error_reply(message.method(), messenger::errno::EACCES);
        }
        if !surface.is_window() {
            return error_reply(message.method(), messenger::errno::EINVAL);
        }
        if let Some(surface) = self.surfaces.iter_mut().find(|s| s.id == args.surface) {
            surface.ask_close = args.ask;
            // The client answered the last question (it stays): ask afresh.
            surface.close_asked = None;
        }
        empty_reply(message.method())
    }
}

/// How long an app has to answer a close request before a second one wins.
pub(super) const GRACE_NS: u64 = 3_000_000_000;

/// What closing a surface does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Step {
    /// Tell the client and drop the surface (the default).
    Drop,
    /// Tell the client and keep the surface until it answers.
    Ask,
    /// The client was asked more than the grace ago: drop it regardless.
    Force,
    /// Asked moments ago: ignore the repeat.
    Hold,
}

/// The pure rule behind `close_surface`.
pub(super) fn close_step(ask_close: bool, asked_at: Option<u64>, now: u64) -> Step {
    match (ask_close, asked_at) {
        (false, _) => Step::Drop,
        (true, None) => Step::Ask,
        (true, Some(at)) if now.saturating_sub(at) >= GRACE_NS => Step::Force,
        (true, Some(_)) => Step::Hold,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_drops() {
        assert_eq!(close_step(false, None, 5), Step::Drop);
        assert_eq!(close_step(false, Some(1), 9 * GRACE_NS), Step::Drop);
    }

    #[test]
    fn asks_once_then_holds_then_forces() {
        assert_eq!(close_step(true, None, 100), Step::Ask);
        assert_eq!(close_step(true, Some(100), 100 + GRACE_NS - 1), Step::Hold);
        assert_eq!(close_step(true, Some(100), 100 + GRACE_NS), Step::Force);
        // A clock that went backwards never wedges the window.
        assert_eq!(close_step(true, Some(100), 50), Step::Hold);
    }
}
