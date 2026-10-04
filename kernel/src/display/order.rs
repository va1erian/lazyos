//! The screen buffer's byte order (ops 7 and 8; docs/performance-plan.md P3.2).
//!
//! The screen buffer is RGBA after every bind, and `present` converts each
//! pixel to the framebuffer's packing. A compositor that composes in the
//! framebuffer's own order instead (op 8 says which, op 7 declares it) gets a
//! plain row copy: the conversion moves out of the interrupts-off syscall and
//! into the compositor's preemptible composition. Correctness never depends
//! on the choice: `present` converts whatever does not match.

use core::sync::atomic::{AtomicU8, Ordering};

use super::{errno, layout as code, negative, NO_OWNER, OWNER};
use crate::gfx::Layout;
use crate::{console, task};

/// The declared layout: 0 RGBA, 1 BGRA (the [`code`] values).
static DECLARED: AtomicU8 = AtomicU8::new(0);

/// Back to RGBA (a new bind, the test reset).
pub(super) fn reset() {
    DECLARED.store(code::RGBA as u8, Ordering::Relaxed);
}

/// The layout `present` reads the screen buffer in.
pub(super) fn current() -> Layout {
    match u64::from(DECLARED.load(Ordering::Relaxed)) {
        code::BGRA => Layout::Bgra,
        _ => Layout::Rgba,
    }
}

/// Whether the caller holds the display grant.
fn is_owner() -> bool {
    let owner = OWNER.load(Ordering::Relaxed);
    owner != NO_OWNER && owner == task::current()
}

/// syscall 12 op 7: declare the screen buffer's layout. Owner only
/// (`-EPERM`); an unknown layout is `-EINVAL`.
pub(super) fn set(layout: u64) -> u64 {
    if !is_owner() {
        return negative(errno::EPERM);
    }
    if layout != code::RGBA && layout != code::BGRA {
        return negative(errno::EINVAL);
    }
    DECLARED.store(layout as u8, Ordering::Relaxed);
    0
}

/// syscall 12 op 8: the layout `present` copies as is, or `-ENOENT` when the
/// framebuffer is not a 4-byte RGB or BGR mode (every layout is converted).
/// Owner only (`-EPERM`).
pub(super) fn native() -> u64 {
    if !is_owner() {
        return negative(errno::EPERM);
    }
    match console::with_framebuffer(|fb| fb.native_layout()).flatten() {
        Some(Layout::Rgba) => code::RGBA,
        Some(Layout::Bgra) => code::BGRA,
        None => negative(errno::ENOENT),
    }
}
