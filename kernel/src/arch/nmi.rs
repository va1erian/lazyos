//! The NMI hang report (issue #382).
//!
//! A kernel that spins on a lock with interrupts off never runs its timer
//! again, so a hang is silent: the serial log just stops. A non-maskable
//! interrupt still gets through, and the test harness raises one when a boot
//! times out (`tools/screenshot/qemu_session.py`, QMP `inject-nmi`; the QEMU
//! monitor's `nmi` does the same by hand). The handler prints what the
//! machine was doing, as `HANG:` lines:
//!
//! * `HANG:CPU`: the interrupted context (a spin shows up as the same `rip`
//!   in a lock's loop, with `IF` clear);
//! * `HANG:LOCKS`: which of the kernel's global spin locks are held;
//! * `HANG:LASTTICK`/`HANG:TASK`: the scheduler's view (`task::diag`);
//! * `HANG:STACK`: raw stack words above a ring-0 context. The kernel is a
//!   PIE the bootloader loads at its logged `virtual_address_offset`;
//!   `anchor` is this handler's runtime address, so a word minus that offset
//!   is an address `addr2line` resolves against the kernel ELF.
//!
//! It then returns: an NMI on a machine that was only slow costs nothing.
//! Everything here avoids locks and the heap (it may interrupt their holder),
//! and writes through [`RawSerial`].

use core::fmt::{self, Write};

use x86_64::registers::control::Cr3;
use x86_64::structures::idt::InterruptStackFrame;

use super::raw_serial::RawSerial;

/// Stack words printed above the interrupted context.
const CPU_STACK_WORDS: usize = 48;
/// Bytes per page: stack dumps stop at the end of the page holding `rsp`,
/// the only memory above it known to be mapped.
const PAGE: u64 = 4096;
/// `RFLAGS.IF`.
const IF: u64 = 1 << 9;

/// A named "is this lock held now" probe.
type LockProbe = (&'static str, fn() -> bool);

/// The kernel's global spin locks worth reporting.
const LOCKS: [LockProbe; 5] = [
    ("tasks", crate::task::diag::table_locked),
    ("heap", crate::mem::heap_locked),
    ("console", crate::console::locked),
    ("serial", crate::serial::locked),
    ("signals", crate::task::signal::registry_locked),
];

/// IDT entry for vector 2.
pub extern "x86-interrupt" fn nmi_handler(frame: InterruptStackFrame) {
    // A report that fails half-way (it cannot: `RawSerial` never errors)
    // still leaves the lines already printed.
    let _ = report(&mut RawSerial, &frame);
}

/// Print the whole report for the context `frame` interrupted.
fn report(out: &mut impl Write, frame: &InterruptStackFrame) -> fmt::Result {
    let anchor = nmi_handler as *const () as u64;
    writeln!(
        out,
        "\nHANG:BEGIN reason=nmi ticks={} current={} anchor={anchor:#x}",
        crate::task::ticks(),
        crate::task::current()
    )?;
    let (rip, rsp) = (
        frame.instruction_pointer.as_u64(),
        frame.stack_pointer.as_u64(),
    );
    let (cs, rflags) = (frame.code_segment.0, frame.cpu_flags.bits());
    writeln!(
        out,
        "HANG:CPU rip={rip:#x} cs={cs:#x} rflags={rflags:#x} if={} rsp={rsp:#x} cr3={:#x}",
        u8::from(rflags & IF != 0),
        Cr3::read().0.start_address().as_u64()
    )?;
    write_locks(out)?;
    crate::task::diag::write_report(out)?;
    if cs & 3 == 0 {
        write_stack(out, crate::task::current(), rsp, CPU_STACK_WORDS)?;
    }
    writeln!(out, "HANG:END")
}

fn write_locks(out: &mut impl Write) -> fmt::Result {
    write!(out, "HANG:LOCKS")?;
    for (name, held) in LOCKS {
        write!(out, " {name}={}", if held() { "held" } else { "free" })?;
    }
    writeln!(out)
}

/// Print up to `max` words from `rsp` upward, four per line, stopping at the
/// end of `rsp`'s page. `rsp` must be a ring-0 stack pointer of a live
/// context (the NMI's own frame, or one the scheduler saved): its page holds
/// that stack's newest words, so it is mapped. A page-aligned `rsp` may sit
/// at the very top of its stack, with nothing mapped above, so it is skipped.
pub fn write_stack(out: &mut impl Write, slot: usize, rsp: u64, max: usize) -> fmt::Result {
    if rsp.is_multiple_of(PAGE) || !rsp.is_multiple_of(8) {
        return writeln!(out, "HANG:STACK slot={slot} rsp={rsp:#x} (not dumped)");
    }
    let words = (((PAGE - rsp % PAGE) / 8) as usize).min(max);
    for line in (0..words).step_by(4) {
        let base = rsp + line as u64 * 8;
        write!(out, "HANG:STACK slot={slot} {base:#x}:")?;
        for index in line..(line + 4).min(words) {
            // SAFETY: `index < words` keeps the read inside `rsp`'s page,
            // which the contract above guarantees is mapped stack memory.
            let word = unsafe { core::ptr::read_volatile((rsp + index as u64 * 8) as *const u64) };
            write!(out, " {word:#018x}")?;
        }
        writeln!(out)?;
    }
    Ok(())
}
