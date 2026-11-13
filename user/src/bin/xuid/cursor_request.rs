//! `SetCursor` (`os.lazy.display.cursor.v1`, docs/cursor-plan.md): an app
//! picks the pointer shape shown over its own surface's content.
//!
//! The scope is authorized twice. The kernel's label rules decide whether an
//! app may send the interface at all (before this code runs); here the
//! compositor checks what the kernel cannot: the surface exists, the sender
//! owns it, and the shape is one it can draw.

use cursorgfx::Shape;
use user::messenger;

use super::surface::Surface;

/// Apply `SetCursor { surface, shape }` from `sender`: the errno to refuse
/// with, or whether the surface's shape changed.
pub(super) fn apply_set_cursor(
    surfaces: &mut [Surface],
    sender: u64,
    id: u64,
    shape: u32,
) -> Result<bool, i64> {
    let Some(surface) = surfaces.iter_mut().find(|s| s.id == id) else {
        return Err(messenger::errno::ENOENT);
    };
    if surface.owner != sender {
        return Err(messenger::errno::EACCES);
    }
    let Some(shape) = Shape::from_wire(shape) else {
        return Err(messenger::errno::EINVAL);
    };
    let changed = surface.cursor != shape;
    surface.cursor = shape;
    Ok(changed)
}

/// Boot check of the `SetCursor` rules: `XUID:CURSORREQ:PASS` or
/// `XUID:CURSORREQ:FAIL <case>`.
pub(super) fn selftest_cursor_request() -> &'static str {
    let mut surfaces = alloc::vec![super::window::test_surface(7, false, false)];
    surfaces[0].owner = 5;
    let call = |surfaces: &mut [Surface], sender, surface, shape| {
        apply_set_cursor(surfaces, sender, surface, shape)
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
