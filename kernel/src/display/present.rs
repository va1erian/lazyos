//! syscall 12 op 3 (`present`): blit a damage rectangle from the owner's
//! screen buffer onto the real framebuffer.
//!
//! The owner can unmap its screen buffer (a Linux compositor has `munmap`),
//! so the kernel must re-validate before every read. It validates only the
//! rows the blit reads, not the whole screen-sized grant: a 1280x720 buffer
//! is ~900 pages, and walking all of them with interrupts off on every
//! cursor-sized present was the cost reported in issue #340.

use core::sync::atomic::Ordering;

use super::{errno, negative, GRANT, NO_OWNER, OWNER};
use crate::{console, task, user_ptr};

/// Bytes per pixel of the screen buffer (RGBA).
const PIXEL_BYTES: usize = 4;

/// The byte range of a `width`-pixel-wide RGBA buffer that holds rows
/// `y..y + h`: `(offset, len)`. `None` on overflow; callers clamp `y`/`h` to
/// the screen first, so for a real grant this always fits inside it.
pub fn damage_rows(width: usize, y: usize, h: usize) -> Option<(usize, usize)> {
    let row = width.checked_mul(PIXEL_BYTES)?;
    Some((y.checked_mul(row)?, h.checked_mul(row)?))
}

/// Unpack `x | y << 16 | w << 32 | h << 48`, every field a `u16`.
fn unpack(packed: u64) -> (usize, usize, usize, usize) {
    let field = |shift: u32| ((packed >> shift) & 0xffff) as usize;
    (field(0), field(16), field(32), field(48))
}

/// syscall 12 op 3: blit a damage rectangle from the screen buffer to the
/// real framebuffer. The rectangle is clamped to the screen; only the owner
/// may present; an unmapped damage range is `-EFAULT`.
pub(super) fn present(packed: u64) -> u64 {
    let owner = OWNER.load(Ordering::Relaxed);
    if owner == NO_OWNER || owner != task::current() {
        return negative(errno::EPERM);
    }
    let (x, y, w, h) = unpack(packed);
    let (va, size, width, height) = {
        let grant = GRANT.lock();
        let Some(grant) = grant.as_ref() else {
            return negative(errno::EPERM);
        };
        (
            grant.va,
            grant.size as usize,
            grant.width as usize,
            grant.height as usize,
        )
    };
    if x >= width || y >= height || w == 0 || h == 0 {
        return 0;
    }
    let w = w.min(width - x);
    let h = h.min(height - y);
    // Only rows `y..y + h` are read. Refuse (rather than trust) a span that
    // would leave the grant, so the kernel never reads past the buffer even
    // if the geometry and size ever disagreed.
    let Some((offset, len)) = damage_rows(width, y, h) else {
        return negative(errno::EFAULT);
    };
    if offset.checked_add(len).is_none_or(|end| end > size) {
        return negative(errno::EFAULT);
    }
    // The buffer is mapped at `va` in the active (caller's) address space, but
    // may have been unmapped since `bind`: validate exactly the span read.
    let Ok(rows) = user_ptr::try_bytes(va + offset as u64, len) else {
        return negative(errno::EFAULT);
    };
    // `rows` starts at screen row `y`, so the source row is 0 and it is `h`
    // rows tall; the destination stays at `(x, y)`.
    // The logical screen is a clipped view of the framebuffer at its centring
    // offset: `(x, y)` is relative to it, and the view cannot be written past.
    console::with_screen(|fb| fb.blit_rgba_region(rows, width, h, x, 0, x, y, w, h));
    0
}
