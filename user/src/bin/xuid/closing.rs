//! `SetCloseMode` (method 48): an app that must ask before it closes keeps its
//! window when the window manager asks it to (`WindowClose`). Only the creator
//! may set it, and it only applies to windows (not the desktop or panels).
//!
//! An app must never make its window unclosable. The rule is on the time of
//! the last close request that was passed on, and nothing the app does moves
//! it: a close is passed on when none was in the last [`EXPIRY_NS`]; a repeat
//! within [`GRACE_NS`] is ignored; a repeat after that, but within the expiry,
//! forgets the surface whatever the app says. A hung or hostile client (even one
//! that calls `SetCloseMode` again and again) cannot keep a window from a user
//! who closes it twice. A shutdown or logout never goes through here: `init`
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
        }
        empty_reply(message.method())
    }
}

/// How long an app has to answer a close request before a second one wins.
pub(super) const GRACE_NS: u64 = 3_000_000_000;

/// After this long a close request is forgotten: the next one asks again (an
/// app whose user cancelled its prompt gets to ask once more).
pub(super) const EXPIRY_NS: u64 = 30_000_000_000;

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
        (true, Some(at)) => match now.saturating_sub(at) {
            age if age < GRACE_NS => Step::Hold,
            age if age < EXPIRY_NS => Step::Force,
            _ => Step::Ask,
        },
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
        assert_eq!(
            close_step(true, Some(100), 100 + EXPIRY_NS - 1),
            Step::Force
        );
        assert_eq!(close_step(true, Some(100), 100 + EXPIRY_NS), Step::Ask);
        // A clock that went backwards never wedges the window.
        assert_eq!(close_step(true, Some(100), 50), Step::Hold);
    }
}
