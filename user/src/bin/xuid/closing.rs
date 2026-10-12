//! `SetCloseMode` (method 48): an app that must ask before it closes keeps its
//! window when the window manager asks it to (`WindowClose`). Only the creator
//! may set it, and it only applies to windows (not the desktop or panels).

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
