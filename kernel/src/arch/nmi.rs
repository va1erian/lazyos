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
//!
//! **Nested NMIs** (docs/architecture/smp.md, "NMIs"). The handler runs on
//! its own IST stack, and the CPU blocks further NMIs only until the next
//! `iretq` — any exception inside the handler (a `#PF` on a stack word it
//! dumps) ends in one. A second NMI would then land on the same IST top and
//! overwrite the first one's frame. So the gate first copies the hardware
//! frame out of the IST top onto the rest of the NMI stack and runs there;
//! an NMI whose interrupted `rsp` lies inside this CPU's NMI stack has
//! interrupted the handler, and is dropped (counted in [`nested_dropped`])
//! with an `iretq` straight from the IST top, which the outer one no longer
//! uses. A dropped NMI loses only a second hang report.

use core::arch::global_asm;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::percpu::NMI_STACK_SIZE;

#[cfg(lazyos_tests)]
pub mod test;

use x86_64::registers::control::Cr3;
use x86_64::structures::idt::InterruptStackFrameValue;

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
const LOCKS: [LockProbe; 6] = [
    ("tasks", crate::task::diag::table_locked),
    ("heap", crate::mem::heap_locked),
    ("console", crate::console::locked),
    ("serial", crate::serial::locked),
    ("signals", crate::task::signal::registry_locked),
    // A yielding lock (`task::relax`): held while its holder parks, so a
    // hang with it held points at whoever cannot get back to release it.
    ("vfs", crate::fs::vfs_locked),
];

/// The context an NMI interrupted, as the CPU saved it.
#[derive(Clone, Copy, Debug)]
pub struct Interrupted {
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
}

/// NMIs dropped because they interrupted this handler (all CPUs).
static NESTED: AtomicU64 = AtomicU64::new(0);

/// NMIs dropped as nested since boot.
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn nested_dropped() -> u64 {
    NESTED.load(Ordering::Relaxed)
}

// IDT entry for vector 2, on `gdt::NMI_IST`. On entry `rsp` is the hardware
// frame at the IST top (RIP, CS, RFLAGS, RSP, SS). The nested test: the
// interrupted RSP within `[top - SIZE + 16, top)`, with `top = frame + 40`
// (the CPU aligned the IST top down to 16 before its five pushes), as one
// unsigned compare, and only from ring 0 (ring 3 may point its RSP anywhere;
// the `syscall` entry's swap window, ring 0 on a user RSP, still could, at
// the cost of one dropped hang report). Otherwise the frame is copied 64 bytes below and the
// rest is `arch::gsgate`'s paranoid gate (an NMI lands anywhere, swap windows
// included).
global_asm!(
    concat!(
        ".global nmi_gate\n",
        "nmi_gate:\n",
        "push rax\n push rcx\n",
        "lea rax, [rsp + 16]\n",
        "test byte ptr [rax + 8], 3\n jnz 4f\n",
        "mov rcx, [rax + 24]\n sub rcx, rax\n add rcx, {lo_bias}\n cmp rcx, {span}\n jb 3f\n4:\n",
        "lea rsp, [rax - 64]\n and rsp, -16\n",
        "push qword ptr [rax + 32]\n push qword ptr [rax + 24]\n push qword ptr [rax + 16]\n",
        "push qword ptr [rax + 8]\n push qword ptr [rax]\n",
        "mov rcx, [rax - 16]\n mov rax, [rax - 8]\n",
        crate::arch::gsgate::gate_save!(),
        "xor ebx, ebx\n mov ecx, 0xC0000101\n rdmsr\n test edx, edx\n js 1f\n",
        "swapgs\n mov ebx, 1\n1:\n",
        "lea rdi, [rsp + 88]\n",
        crate::arch::gsgate::gate_call!(),
        "test ebx, ebx\n jz 2f\n swapgs\n2:\n",
        crate::arch::gsgate::gate_restore!(),
        "iretq\n",
        "3:\n lock inc qword ptr [rip + {nested}]\n pop rcx\n pop rax\n iretq\n"
    ),
    lo_bias = const NMI_STACK_SIZE - 56,
    span = const NMI_STACK_SIZE - 16,
    nested = sym NESTED,
    target = sym nmi_handler,
);

extern "C" {
    pub(crate) fn nmi_gate();
}

/// The NMI: print the hang report for the interrupted context.
extern "C" fn nmi_handler(frame: &InterruptStackFrameValue) {
    let cpu = crate::arch::percpu::current();
    cpu.ipi.nmis.fetch_add(1, Ordering::Relaxed);
    #[cfg(lazyos_tests)]
    if test::intercept(cpu) {
        return;
    }
    let at = Interrupted {
        rip: frame.instruction_pointer.as_u64(),
        cs: frame.code_segment.0.into(),
        rflags: frame.cpu_flags.bits(),
        rsp: frame.stack_pointer.as_u64(),
    };
    // A report that fails half-way (it cannot: `RawSerial` never errors)
    // still leaves the lines already printed.
    let _ = report(&mut RawSerial, &at);
    REPORTS.fetch_add(1, Ordering::Release);
}

/// Hang reports printed to the end since boot (all CPUs).
static REPORTS: AtomicU64 = AtomicU64::new(0);

/// Hang reports printed to the end since boot: a CPU that asked another for
/// one (the entry lock's watchdog) waits for this to move before panicking.
#[cfg_attr(not(lazyos_smp), allow(dead_code))]
pub fn reports_done() -> u64 {
    REPORTS.load(Ordering::Acquire)
}

/// Print the whole report for the context `at` (the NMI's interrupted one).
pub fn report(out: &mut impl Write, at: &Interrupted) -> fmt::Result {
    let anchor = nmi_handler as *const () as u64;
    writeln!(
        out,
        "\nHANG:BEGIN reason=nmi ticks={} current={} anchor={anchor:#x}",
        crate::task::ticks(),
        crate::task::current()
    )?;
    writeln!(
        out,
        "HANG:CPU rip={:#x} cs={:#x} rflags={:#x} if={} rsp={:#x} cr3={:#x}",
        at.rip,
        at.cs,
        at.rflags,
        u8::from(at.rflags & IF != 0),
        at.rsp,
        Cr3::read().0.start_address().as_u64()
    )?;
    write_locks(out)?;
    // The kernel entry lock (S8.3): which CPU runs kernel code.
    match crate::sync::entry::holder() {
        Some(cpu) => writeln!(out, "HANG:ENTRY holder=CPU{cpu}")?,
        None => writeln!(out, "HANG:ENTRY holder=none")?,
    }
    crate::task::diag::write_report(out)?;
    if at.cs & 3 == 0 {
        write_stack(out, crate::task::current(), at.rsp, CPU_STACK_WORDS)?;
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
