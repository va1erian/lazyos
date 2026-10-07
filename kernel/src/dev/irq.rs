//! Interrupt entry for device lines (issue #240, driver-plan D2/section 3.3).
//!
//! The vector stubs for IRQ 0-15 (`arch::irq_stubs`) land in [`dispatch`]. It
//! runs in interrupt context on a single CPU, so it may **not** take a lock,
//! allocate, or touch a queue: whatever it interrupted may hold the heap, the
//! channel registry or the claim table, and spinning on it would hang the
//! machine. It therefore only
//!
//! 1. answers a spurious IRQ 7/15,
//! 2. runs a kernel driver's plain `fn(line)` if one is registered, or
//! 3. masks the line, sets an atomic "raised" bit and sends EOI.
//!
//! The line is masked at whichever controller delivers it (`arch::irqchip`:
//! the 8259, or the I/O APIC since issue #616). MSI vectors have their own
//! lock-free handler (`dev::msi::dispatch`) and raise the same bit set, as
//! delivery *sources* past the sixteen lines.
//!
//! The bottom half (`dev::intx::service`) turns the raised bit into one-way
//! Messenger messages from the kernel identity. It may lock and allocate, so
//! it never runs inside [`dispatch`]; the line stub runs it right after,
//! still in the interrupt, when the interrupt stopped user code or a task
//! halted in `nap` (nothing that could hold its locks, P1.2), and otherwise
//! it runs at the next syscall, the next tick that lands in such code, or the
//! kernel task's loop. Masking the line first is what makes level-triggered
//! INTx safe: the device keeps asserting, but the controller cannot
//! re-deliver until a claimant acknowledges.
//!
//! Lines 0 (timer), 1 (keyboard), 2 (cascade) and 12 (mouse) keep their own
//! handlers and can never be claimed.

use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::arch::irqchip;

use super::errno::{Errno, EBUSY, EINVAL};

/// Number of legacy lines.
pub const LINES: u8 = 16;
/// Delivery sources: the legacy lines, then one per MSI vector.
pub const SOURCES: u8 = LINES + super::msi::VECTORS;

/// Lines owned by the kernel's built-in handlers: never claimable.
pub const RESERVED: u16 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 12);

/// Whether a device interrupt on `line` can be delivered to a claimant. A PCI
/// function whose Interrupt Line register is 0xFF ("not connected"), out of
/// range, or one of the [`RESERVED`] lines is not routable, and its driver
/// falls back to polling (`irq_enable` returns `ENOSYS`).
pub const fn routable(line: u8) -> bool {
    line < LINES && RESERVED & (1 << line) == 0
}

/// Sources that fired and await the bottom half.
static RAISED: AtomicU64 = AtomicU64::new(0);

const _: () = assert!(SOURCES as u32 <= u64::BITS, "one raised bit per source");
/// Kernel drivers' `fn(u8)` handlers by line; 0 means none.
static KERNEL_HANDLERS: [AtomicUsize; LINES as usize] = [const { AtomicUsize::new(0) }; 16];

static SPURIOUS: AtomicU32 = AtomicU32::new(0);
static STRAY: AtomicU32 = AtomicU32::new(0);
static RAISES: AtomicU32 = AtomicU32::new(0);

/// Counters for diagnostics and the suite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IrqStats {
    /// Real interrupts that were masked and queued for the bottom half.
    pub raised: u32,
    /// Spurious IRQ 7/15 the 8259 raised and the handler discarded.
    pub spurious: u32,
    /// Interrupts on a line nobody claimed (masked so they cannot storm).
    pub stray: u32,
}

pub fn stats() -> IrqStats {
    IrqStats {
        raised: RAISES.load(Ordering::Relaxed),
        spurious: SPURIOUS.load(Ordering::Relaxed),
        stray: STRAY.load(Ordering::Relaxed),
    }
}

/// The interrupt handler body for `line`. Lock-free by construction.
pub fn dispatch(line: u8) {
    if line == 2 {
        // The cascade is never delivered as an interrupt of its own; a glitch
        // on it is acknowledged and dropped (it cannot be masked: the slave
        // PIC hangs off it).
        // SAFETY: called from the IRQ 2 handler, once.
        unsafe { irqchip::eoi(2) };
        return;
    }
    if !routable(line) {
        // The kernel's own lines never come through here; refuse a stray call
        // rather than mask the timer.
        return;
    }
    // SAFETY: called from the handler of `line`.
    if unsafe { irqchip::spurious(line) } {
        SPURIOUS.fetch_add(1, Ordering::Relaxed);
        if line == 15 {
            // The slave raised it, so the master saw IRQ2 in service and needs
            // its EOI; the slave gets none for a spurious interrupt.
            // SAFETY: called from the IRQ 15 handler, once.
            unsafe { crate::arch::pic::end_of_interrupt_specific(2) };
        }
        return;
    }
    if let Some(handler) = kernel_handler(line) {
        handler(line);
    } else {
        // Claimed by a userspace driver, or nobody's: either way hold the line
        // until the bottom half decides. An unclaimed line stays masked, so a
        // device nobody drives cannot storm.
        irqchip::set_masked(line, true);
        RAISES.fetch_add(1, Ordering::Relaxed);
        raise(line);
    }
    // SAFETY: called from the handler of `line`, once per interrupt.
    unsafe { irqchip::eoi(line) };
}

/// Record that delivery source `source` fired (lock-free; its line or vector
/// is already masked).
pub fn raise(source: u8) {
    if source < SOURCES {
        RAISED.fetch_or(1 << source, Ordering::AcqRel);
        crate::perf::line_raised(source);
    }
}

/// Forget a pending raise of `source` (its vector is being freed).
pub(super) fn clear(source: u8) {
    if source < SOURCES {
        RAISED.fetch_and(!(1 << source), Ordering::AcqRel);
    }
}

/// Sources raised since the last call, for the bottom half.
pub fn take_raised() -> u64 {
    RAISED.swap(0, Ordering::AcqRel)
}

/// Whether a raise waits for the bottom half.
pub fn raised_pending() -> bool {
    RAISED.load(Ordering::Acquire) != 0
}

/// Put `source` back for a later bottom-half pass (it stays masked).
pub(super) fn requeue(source: u8) {
    if source < SOURCES {
        RAISED.fetch_or(1 << source, Ordering::AcqRel);
    }
}

/// Count an interrupt that no claim armed (the bottom half saw no listener).
pub(super) fn note_stray() {
    STRAY.fetch_add(1, Ordering::Relaxed);
}

fn kernel_handler(line: u8) -> Option<fn(u8)> {
    let raw = KERNEL_HANDLERS
        .get(usize::from(line))?
        .load(Ordering::Acquire);
    if raw == 0 {
        return None;
    }
    // SAFETY: the slot only ever holds 0 or a value stored by
    // `register_kernel` from a valid `fn(u8)` pointer, and function pointers
    // and `usize` have the same size on this target.
    Some(unsafe { core::mem::transmute::<usize, fn(u8)>(raw) })
}

/// Whether a kernel driver owns `line`.
pub fn has_kernel_handler(line: u8) -> bool {
    kernel_handler(line).is_some()
}

/// Register a kernel driver's plain handler for `line` and unmask it.
///
/// The handler runs in interrupt context: no locks, no allocation, no
/// blocking; it must quiet the device itself. Fails with `EINVAL` for a
/// reserved or out-of-range line and `EBUSY` if the line already has a kernel
/// handler or a userspace claimant is armed on it.
pub fn register_kernel(line: u8, handler: fn(u8)) -> Result<(), Errno> {
    if !routable(line) {
        return Err(EINVAL);
    }
    if super::intx::line_in_use(line) {
        return Err(EBUSY);
    }
    let slot = &KERNEL_HANDLERS[usize::from(line)];
    slot.compare_exchange(0, handler as usize, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| EBUSY)?;
    irqchip::set_masked(line, false);
    Ok(())
}

/// Remove a kernel driver's handler and mask the line again.
pub fn unregister_kernel(line: u8) {
    if let Some(slot) = KERNEL_HANDLERS.get(usize::from(line)) {
        irqchip::set_masked(line, true);
        slot.store(0, Ordering::Release);
    }
}
