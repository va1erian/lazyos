//! syscall 12 op 3 (`present`): blit a damage rectangle from the owner's
//! screen buffer onto the real framebuffer.
//!
//! The owner can unmap its screen buffer (a Linux compositor has `munmap`),
//! so the kernel must re-validate before every read. It validates only the
//! rows the blit reads, not the whole screen-sized grant: a 1280x720 buffer
//! is ~900 pages, and walking all of them with interrupts off on every
//! cursor-sized present was the cost reported in issue #340.
//!
//! **Chunks** (docs/performance-plan.md P3.2). Syscalls run with interrupts
//! off and the console lock is only ever held with them off (issue #382), so
//! a full-screen present used to keep every interrupt waiting for the whole
//! copy. The rectangle is now copied [`CHUNK_BYTES`] of screen rows at a
//! time: each chunk takes the console lock, validates exactly its own rows
//! and copies them; between chunks the lock is released and interrupts are
//! let in ([`breathe`]), exactly as a syscall's `nap` does, so a pending
//! interrupt waits at most one chunk. That window may switch tasks, and the
//! owner's other threads may unmap the buffer meanwhile, which is why the
//! grant and the rows are re-read for every chunk: nothing read before the
//! window is trusted after it. A cursor-sized present is one chunk and never
//! opens a window.

use core::sync::atomic::Ordering;

use super::{errno, negative, order, GRANT, NO_OWNER, OWNER};
use crate::{console, task, user_ptr};

/// Bytes per pixel of the screen buffer.
const PIXEL_BYTES: usize = 4;

/// Screen-buffer bytes copied per console-lock hold: about 15 µs of `rep
/// movs` on current hardware, 25 rows of a 1280-pixel screen.
pub const CHUNK_BYTES: usize = 128 * 1024;

/// The byte range of a `width`-pixel-wide RGBA buffer that holds rows
/// `y..y + h`: `(offset, len)`. `None` on overflow; callers clamp `y`/`h` to
/// the screen first, so for a real grant this always fits inside it.
pub fn damage_rows(width: usize, y: usize, h: usize) -> Option<(usize, usize)> {
    let row = width.checked_mul(PIXEL_BYTES)?;
    Some((y.checked_mul(row)?, h.checked_mul(row)?))
}

/// Screen rows per chunk for a `width`-pixel-wide buffer (at least one).
pub fn chunk_rows(width: usize) -> usize {
    (CHUNK_BYTES / width.saturating_mul(PIXEL_BYTES).max(1)).max(1)
}

/// Unpack `x | y << 16 | w << 32 | h << 48`, every field a `u16`.
fn unpack(packed: u64) -> (usize, usize, usize, usize) {
    let field = |shift: u32| ((packed >> shift) & 0xffff) as usize;
    (field(0), field(16), field(32), field(48))
}

/// The grant as `present` needs it: `(va, size, width, height)`, or `None`
/// when the caller does not hold it.
fn grant() -> Option<(u64, usize, usize, usize)> {
    let owner = OWNER.load(Ordering::Relaxed);
    if owner == NO_OWNER || owner != task::current() {
        return None;
    }
    let grant = GRANT.lock();
    grant.as_ref().map(|grant| {
        (
            grant.va,
            grant.size as usize,
            grant.width as usize,
            grant.height as usize,
        )
    })
}

/// syscall 12 op 3: blit a damage rectangle from the screen buffer to the
/// real framebuffer. The rectangle is clamped to the screen; only the owner
/// may present; an unmapped damage range is `-EFAULT` (the chunks above it
/// were already shown).
pub(super) fn present(packed: u64) -> u64 {
    let Some((_, _, width, height)) = grant() else {
        return negative(errno::EPERM);
    };
    let (x, y, w, h) = unpack(packed);
    if x >= width || y >= height || w == 0 || h == 0 {
        return 0;
    }
    let w = w.min(width - x);
    let end = y + h.min(height - y);
    let step = chunk_rows(width);
    let mut row = y;
    while row < end {
        let rows = step.min(end - row);
        match present_rows(x, w, row, rows, (width, height)) {
            Chunk::Shown => {}
            Chunk::Fault => return negative(errno::EFAULT),
            // The grant went away or changed shape while interrupts were on:
            // what is left belongs to a screen that no longer exists.
            Chunk::Gone => return 0,
        }
        row += rows;
        if row < end {
            breathe();
        }
    }
    0
}

/// How one chunk went.
enum Chunk {
    Shown,
    Fault,
    Gone,
}

/// Copy screen rows `row..row + rows`, columns `x..x + w`, if the grant still
/// has the `shape` the present was clamped against.
fn present_rows(x: usize, w: usize, row: usize, rows: usize, shape: (usize, usize)) -> Chunk {
    let Some((va, size, width, height)) = grant() else {
        return Chunk::Gone;
    };
    if (width, height) != shape {
        return Chunk::Gone;
    }
    // Refuse (rather than trust) a span that would leave the grant, so the
    // kernel never reads past the buffer even if geometry and size disagreed.
    let Some((offset, len)) = damage_rows(width, row, rows) else {
        return Chunk::Fault;
    };
    if offset.checked_add(len).is_none_or(|end| end > size) {
        return Chunk::Fault;
    }
    // The buffer is mapped at `va` in the active (caller's) address space, but
    // may have been unmapped since `bind` or the last chunk: validate exactly
    // the span read, now, with interrupts off until the copy is done.
    let Ok(source) = user_ptr::try_bytes(va + offset as u64, len) else {
        return Chunk::Fault;
    };
    let layout = order::current();
    // `source` starts at screen row `row`, so the source row is 0; the
    // destination is `(x, row)` of the logical screen, a clipped view of the
    // framebuffer at its centring offset that cannot be written past.
    console::with_screen(|fb| fb.blit_region(source, layout, width, rows, x, 0, x, row, w, rows));
    Chunk::Shown
}

/// Between two chunks: let pending interrupts in, deliver what they raised
/// to userspace drivers, and give the CPU to a task they woke if it should
/// run first (P1.1). Nothing is held here: no lock, no user slice.
///
/// The kernel suite runs without a scheduler and with interrupts off, on a
/// faked current task, so there a breath only counts and calls the test's
/// hook (which plays what another thread could do meanwhile).
fn breathe() {
    if cfg!(lazyos_tests) {
        #[cfg(lazyos_tests)]
        test_hooks::breath();
        return;
    }
    crate::perf::irqoff_pause();
    x86_64::instructions::interrupts::enable();
    // `sti` takes effect after the next instruction, so a pending interrupt
    // is taken right after this one.
    core::hint::spin_loop();
    x86_64::instructions::interrupts::disable();
    crate::perf::irqoff_resume();
    crate::dev::intx::service();
    task::preempt_point();
}

/// What the kernel suite observes of [`breathe`].
#[cfg(lazyos_tests)]
pub mod test_hooks {
    use core::sync::atomic::{AtomicUsize, Ordering};
    use spin::Mutex;

    static BREATHS: AtomicUsize = AtomicUsize::new(0);
    static HOOK: Mutex<Option<fn(usize)>> = Mutex::new(None);

    /// One breath: count it and run the hook with its 1-based number.
    pub(super) fn breath() {
        let number = BREATHS.fetch_add(1, Ordering::Relaxed) + 1;
        let hook = *HOOK.lock();
        if let Some(hook) = hook {
            hook(number);
        }
    }

    /// Breaths since the last call, and reset the count.
    pub fn take_breaths() -> usize {
        BREATHS.swap(0, Ordering::Relaxed)
    }

    /// Run `hook` at every breath (`None`: nothing).
    pub fn set_hook(hook: Option<fn(usize)>) {
        *HOOK.lock() = hook;
    }
}
