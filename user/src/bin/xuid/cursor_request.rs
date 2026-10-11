//! `SetCursor` (`os.lazy.display.cursor.v1`, docs/cursor-plan.md): an app
//! picks the pointer shape shown over its own surface's content.
//!
//! The scope is authorized twice. The kernel's label rules decide whether an
//! app may send the interface at all (before this code runs); here the
//! compositor checks what the kernel cannot: the surface exists, the sender
//! owns it, and the shape is one it can draw. Any other method of the
//! interface is refused.

use cursorgfx::Shape;
use libmessenger::Parcel;
use messenger_generated::os_lazy_display_cursor_v1 as wire;
use user::messenger::{self, Message};

use super::compositor::Compositor;
use super::protocol::{empty_reply, error_reply};
use super::surface::Surface;

/// Whether `message` is for the cursor scope rather than the display
/// interface proper.
pub(super) fn is_cursor_scope(message: &Message) -> bool {
    message.interface_id() == wire::INTERFACE_ID
}

/// Apply `SetCursor { surface, shape }` from `sender`: the errno to refuse
/// with, or whether the surface's shape changed.
pub(super) fn apply_set_cursor(
    surfaces: &mut [Surface],
    sender: u64,
    args: &wire::SetCursorArgs,
) -> Result<bool, i64> {
    let Some(surface) = surfaces.iter_mut().find(|s| s.id == args.surface) else {
        return Err(messenger::errno::ENOENT);
    };
    if surface.owner != sender {
        return Err(messenger::errno::EACCES);
    }
    let Some(shape) = Shape::from_wire(args.shape) else {
        return Err(messenger::errno::EINVAL);
    };
    let changed = surface.cursor != shape;
    surface.cursor = shape;
    Ok(changed)
}

impl Compositor {
    /// Handle one request on the cursor scope.
    pub(super) fn cursor_request(&mut self, message: &Message) -> Parcel {
        let method = message.method();
        if method != wire::METHOD_SETCURSOR {
            return error_reply(method, messenger::errno::EINVAL);
        }
        let Ok(args) = wire::decode_set_cursor_args(&message.parcel.body) else {
            return error_reply(method, messenger::errno::EINVAL);
        };
        match apply_set_cursor(&mut self.surfaces, message.sender, &args) {
            Ok(changed) => {
                if changed {
                    // The pointer may be over this surface right now.
                    self.move_cursor();
                }
                empty_reply(method)
            }
            Err(code) => error_reply(method, code),
        }
    }
}

/// Boot check of the `SetCursor` rules: `XUID:CURSORREQ:PASS` or
/// `XUID:CURSORREQ:FAIL <case>`.
pub(super) fn selftest_cursor_request() -> &'static str {
    let mut surfaces = alloc::vec![super::window::test_surface(7, false, false)];
    surfaces[0].owner = 5;
    let call = |surfaces: &mut [Surface], sender, surface, shape| {
        apply_set_cursor(surfaces, sender, &wire::SetCursorArgs { surface, shape })
    };
    if call(&mut surfaces, 5, 99, 3) != Err(messenger::errno::ENOENT) {
        return "XUID:CURSORREQ:FAIL unknown surface\n";
    }
    if call(&mut surfaces, 6, 7, 3) != Err(messenger::errno::EACCES)
        || surfaces[0].cursor != Shape::Default
    {
        return "XUID:CURSORREQ:FAIL not the owner\n";
    }
    if call(&mut surfaces, 5, 7, 8) != Err(messenger::errno::EINVAL)
        || call(&mut surfaces, 5, 7, u32::MAX) != Err(messenger::errno::EINVAL)
    {
        return "XUID:CURSORREQ:FAIL bad shape\n";
    }
    if call(&mut surfaces, 5, 7, 3) != Ok(true) || surfaces[0].cursor != Shape::Hand {
        return "XUID:CURSORREQ:FAIL set hand\n";
    }
    if call(&mut surfaces, 5, 7, 3) != Ok(false) {
        return "XUID:CURSORREQ:FAIL repeat is a no-op\n";
    }
    if call(&mut surfaces, 5, 7, 0) != Ok(true) || surfaces[0].cursor != Shape::Default {
        return "XUID:CURSORREQ:FAIL restore\n";
    }
    "XUID:CURSORREQ:PASS\n"
}
