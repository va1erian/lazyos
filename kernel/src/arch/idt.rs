//! Interrupt descriptor table and handlers.

use crate::arch::fault::Fault;
use crate::arch::fault_storm::Path;
use crate::arch::gsgate::irq_gate;
use crate::arch::pic;
use crate::mem::Resolve;
use crate::task::signal::Exception;
use alloc::boxed::Box;
use core::arch::global_asm;
use core::sync::atomic::AtomicU64;
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{
    InterruptDescriptorTable, InterruptStackFrameValue, PageFaultErrorCode,
};

/// Timer ticks since boot.
pub static TICKS: AtomicU64 = AtomicU64::new(0);

mod faults;
mod table;
pub use table::load_on_this_cpu;

/// Build and load the IDT.
pub fn init() {
    let mut idt = InterruptDescriptorTable::new();
    faults::install(&mut idt);
    // Issue #382: an injected NMI prints a hang report, even over IF=0 spins.
    // Its own IST stack: an NMI can land where `rsp` is still the user's
    // (the `syscall` stub before its stack switch, or after `pop rsp`).
    // SAFETY: `nmi_gate` is a paranoid gate (`arch::gsgate`) for a vector
    // without an error code, ending in `iretq`; `NMI_IST` names the TSS slot
    // `gdt::init_cpu` gave every CPU's `nmi_stack`.
    unsafe {
        idt.non_maskable_interrupt
            .set_handler_addr(gate(super::nmi::nmi_gate))
            .set_stack_index(super::gdt::NMI_IST);
    }
    // Safety: the naked `*_isr` stubs below match each exception's frame
    // layout (`divide_error_isr`/`invalid_opcode_isr` push no error code,
    // `general_protection_isr` and `page_fault_isr` do) and never return a Rust
    // value, only the frame pointer to resume.
    unsafe {
        idt.divide_error
            .set_handler_addr(x86_64::VirtAddr::new(divide_error_isr as *const () as u64));
        idt.invalid_opcode.set_handler_addr(x86_64::VirtAddr::new(
            invalid_opcode_isr as *const () as u64,
        ));
        idt.general_protection_fault
            .set_handler_addr(x86_64::VirtAddr::new(
                general_protection_isr as *const () as u64,
            ));
    }
    // Safety: `page_fault_isr` is a naked handler with the (error-code-pushing)
    // layout #PF uses; it never returns a Rust value, only the frame pointer.
    unsafe {
        idt.page_fault
            .set_handler_addr(x86_64::VirtAddr::new(page_fault_isr as *const () as u64));
    }
    // PIC IRQ0 (timer) drives preemption via the naked ISR in `task::switch`.
    idt[32].set_handler_fn(naked_gate(crate::task::switch::timer_isr as *const ()));
    // Voluntary reschedules (a task parking) use their own vector so they
    // never count as a PIT tick or send a spurious EOI (issue #338). DPL 0:
    // ring 3 cannot raise it.
    idt[crate::task::switch::YIELD_VECTOR]
        .set_handler_fn(naked_gate(crate::task::switch::yield_isr as *const ()));
    // The local APIC timer, when it is the tick (`arch::timer`), enters the
    // same scheduler gate; `timer::end_of_tick` acknowledges the right chip.
    idt[super::lapic::TIMER_VECTOR]
        .set_handler_fn(naked_gate(crate::task::switch::timer_isr as *const ()));
    // The one-shot deadline timer (P2.2), when the PIT is the tick, and the
    // kernel's IPIs (S8.2, docs/architecture/smp.md): reschedule,
    // shootdown and stop — the SMP generalization of the window-safe
    // handlers (no locks, no allocation, no task switch).
    // SAFETY: every address is an `irq_gate!` stub (`arch::gsgate`) for a
    // vector without an error code, ending in `iretq`.
    unsafe {
        idt[super::event_timer::VECTOR]
            .set_handler_addr(gate(super::event_timer::event_timer_gate));
        idt[super::smp::RESCHED_VECTOR].set_handler_addr(gate(super::smp::resched_gate));
        idt[super::smp::SHOOTDOWN_VECTOR].set_handler_addr(gate(super::smp::shootdown_gate));
        idt[super::smp::STOP_VECTOR].set_handler_addr(gate(super::smp::stop_gate));
        idt[super::lapic::SPURIOUS_VECTOR].set_handler_addr(gate(lapic_spurious_gate));
        idt[33].set_handler_addr(gate(keyboard_gate));
        idt[44].set_handler_addr(gate(mouse_gate));
    }
    // Every other PIC line reaches the device core (issue #240), and so do
    // the MSI vectors (issue #616).
    super::irq_stubs::install(&mut idt);
    super::msi_stubs::install(&mut idt);
    // int 0x80: user-mode syscall gate (DPL 3).
    idt[0x80]
        .set_handler_fn(crate::process::syscall_gate())
        .set_privilege_level(x86_64::PrivilegeLevel::Ring3);

    table::publish_and_load(Box::new(idt));
}

/// Initialise interrupt hardware (PIC, PIT, the tick source) and load the IDT.
pub fn init_hardware() {
    // Safety: reprogramming the PIC/PIT is only valid once and before enabling IRQs.
    unsafe {
        pic::init();
        pic::init_pit(100);
    }
    // The PIT stays the tick unless it is found not ticking (or the image
    // forces the local APIC timer); this also calibrates the TSC catch-up.
    super::timer::init();
    // With the tick chosen, move the lines to the I/O APIC (issue #616).
    super::irqchip::init();
    init();
}

/// The IDT address of an `arch::gsgate` stub.
pub(super) fn gate(stub: unsafe extern "C" fn()) -> x86_64::VirtAddr {
    x86_64::VirtAddr::new(stub as *const () as u64)
}

irq_gate!(lapic_spurious_gate => lapic_spurious_handler);
irq_gate!(keyboard_gate => keyboard_handler);
irq_gate!(mouse_gate => mouse_handler);

/// The local APIC's spurious vector: no EOI, by definition (SDM 11.9).
extern "C" fn lapic_spurious_handler(_stack: &InterruptStackFrameValue) {}

// The page fault handler needs the interrupted general registers to build a
// `SIGSEGV` frame, and the `x86-interrupt` ABI does not expose them, so #PF
// (and #DE/#UD/#GP, issue #246) use naked stubs like the timer: push the
// registers, call into Rust with the frame pointer, resume at the (possibly
// rewritten) frame. Like the timer they clear the direction flag first
// (issue #405): a page fault inside a user `memmove` arrives with DF set, and
// the COW/demand-zero paths copy and zero whole frames with `rep movs`/`stos`.
// The `x86-interrupt` handlers need no such care: LLVM emits `cld` for them.
// Frame layout, `cld` and switch-order contract: docs/architecture/tasks.md,
// "Entry stub contract".
//
// `%gs` (`arch::gsgate`): an exception can land inside a swap window, so the
// entry reads `IA32_GS_BASE` (paranoid) after saving rax/rcx/rdx; the exit
// tests the resumed frame's `CS` (a dispatch may resume another task).
global_asm!(concat!(
    r#"
    .macro exception_isr name, vector, has_error
    .global \name
    \name:
        push rax
        push rbx
        push rcx
        push rdx
        push rsi
        push rdi
        push rbp
        push r8
        push r9
        push r10
        push r11
        push r12
        push r13
        push r14
        push r15
"#,
    crate::gs_entry_paranoid!(r"[rsp + 128 + 8 * \has_error]"),
    r#"
        cld                         /* see task::switch: DF may be set */
        mov rdi, rsp
        mov rsi, \vector
        /* the CPU aligns RSP only before its own push: force the ABI's 16 */
        and rsp, -16
        call exception_dispatch
        mov rsp, rax

        pop r15
        pop r14
        pop r13
        pop r12
        pop r11
        pop r10
        pop r9
        pop r8
        pop rbp
        pop rdi
        pop rsi
        pop rdx
        pop rcx
        pop rbx
        pop rax
        .if \has_error
        add rsp, 8                  /* the CPU's error code */
        .endif
"#,
    crate::gs_exit_swap_on_user_cs!(),
    r#"
        iretq
    .endm

    exception_isr divide_error_isr, 0, 0
    exception_isr invalid_opcode_isr, 6, 0
    exception_isr general_protection_isr, 13, 1
    "#
));

// Frame layout, `cld` and switch-order contract: docs/architecture/tasks.md,
// "Entry stub contract".
// `%gs`: as `exception_isr` above.
global_asm!(concat!(
    r#"
    .global page_fault_isr
    page_fault_isr:
        push rax
        push rbx
        push rcx
        push rdx
        push rsi
        push rdi
        push rbp
        push r8
        push r9
        push r10
        push r11
        push r12
        push r13
        push r14
        push r15
"#,
    crate::gs_entry_paranoid!("[rsp + 136]"),
    r#"
        cld                         /* see task::switch: DF may be set */
        mov rdi, rsp
        call page_fault_dispatch
        mov rsp, rax

        pop r15
        pop r14
        pop r13
        pop r12
        pop r11
        pop r10
        pop r9
        pop r8
        pop rbp
        pop rdi
        pop rsi
        pop rdx
        pop rcx
        pop rbx
        pop rax
        add rsp, 8                  /* the CPU's page-fault error code */
"#,
    crate::gs_exit_swap_on_user_cs!(),
    r#"
        iretq
    "#
));

extern "C" {
    fn page_fault_isr();
    fn divide_error_isr();
    fn invalid_opcode_isr();
    fn general_protection_isr();
}

/// Resolve `#DE`/`#UD`/`#GP`: a ring-3 fault enters the process's signal
/// handler when it has one and otherwise ends the process; a ring-0 fault
/// halts with a diagnostic. Returns the frame pointer to resume.
#[no_mangle]
extern "C" fn exception_dispatch(rsp: u64, vector: u64) -> u64 {
    let (exception, fault, name) = match vector {
        0 => (Exception::DivideError, Fault::DivideError, "divide error"),
        6 => (
            Exception::InvalidOpcode,
            Fault::InvalidOpcode,
            "invalid opcode",
        ),
        _ => (
            Exception::GeneralProtection,
            Fault::GeneralProtection,
            "general protection fault",
        ),
    };
    let rip_index = exception.rip_index();
    // SAFETY: `rsp` is the frame `exception_isr` saved: 15 registers, an
    // optional error code, then RIP and CS at `rip_index` and `rip_index + 1`.
    let (rip, cs) = unsafe {
        (
            core::ptr::read_volatile((rsp + rip_index as u64 * 8) as *const u64),
            core::ptr::read_volatile((rsp + (rip_index as u64 + 1) * 8) as *const u64),
        )
    };
    if crate::arch::fault::from_user(cs) && crate::task::signal::deliver_exception(rsp, exception) {
        return rsp;
    }
    // SAFETY: `rsp` is the frame `exception_isr` saved, in the layout
    // `rip_index` describes.
    unsafe {
        crate::arch::fault::contain_frame(rsp, rip_index, fault, format_args!("rip {rip:#x}"))
    };
    serial_println!("EXCEPTION: {name} at {rip:#x}");
    crate::halt();
}

/// Resolve a page fault: COW copy, demand-zero page, or `SIGSEGV` into a
/// handler. Returns the (possibly rewritten) frame pointer to resume; without
/// a handler the diagnostic halt stays, exactly as before signals.
#[no_mangle]
extern "C" fn page_fault_dispatch(rsp: u64) -> u64 {
    // Frame: 15 general registers, then the error code, RIP, CS, RFLAGS, RSP, SS.
    // Safety: `rsp` points at the page-fault frame `page_fault_isr` saved,
    // whose fixed layout puts the CPU-pushed error code at word 15.
    let raw_error = unsafe { core::ptr::read_volatile((rsp + 15 * 8) as *const u64) };
    let error = PageFaultErrorCode::from_bits_truncate(raw_error);
    let addr = Cr2::read();
    // SAFETY: `rsp` is the frame `page_fault_isr` saved; CS is word 17 (after
    // the 15 registers, the error code and RIP).
    let saved_cs = unsafe { core::ptr::read_volatile((rsp + 17 * 8) as *const u64) };
    // A hypervisor-fabricated fault on the emulated `rep insw` (its CR2 and
    // error code are garbage) aborts that transfer; it must be caught before
    // the COW/demand-zero paths act on the bogus CR2.
    if crate::arch::string_io::recover(rsp) {
        return rsp;
    }
    // SAFETY: as above; RIP is word 16, just before CS.
    let rip = unsafe { core::ptr::read_volatile((rsp + 16 * 8) as *const u64) };
    if let Ok(fault) = addr {
        let table = crate::mem::kernel_table();
        // Every "handled" exit resumes the same instruction; a path that is
        // wrong about it livelocks the task, so each one is counted.
        let handled = |path| {
            crate::arch::fault_storm::note(table, rip, fault.as_u64(), raw_error, path);
            rsp
        };
        // A ring-3 fault the page tables already permit is the hypervisor
        // emulator's fabrication (see `arch::spurious_fault`): retry.
        if crate::arch::fault::from_user(saved_cs)
            && crate::arch::spurious_fault::is_spurious(error, fault.as_u64())
        {
            crate::arch::spurious_fault::note(error, fault.as_u64(), rip);
            return handled(Path::Spurious);
        }
        // A write to a present copy-on-write user page takes a private copy.
        // This must come first: such a page is present, so the demand-zero
        // path below would never apply to it.
        let mut starved = false;
        if error.contains(PageFaultErrorCode::CAUSED_BY_WRITE) {
            match crate::mem::cow_resolve(table, fault.as_u64()) {
                Resolve::Handled => return handled(Path::Cow),
                Resolve::OutOfFrames => starved = true,
                Resolve::Refused => {}
            }
        }
        // A not-present page inside an Anon/Heap VMA is demand-zero memory:
        // materialize it (if the VMA permits this access) and resume.
        if !starved {
            match crate::mem::demand_resolve(table, fault.as_u64(), error) {
                Resolve::Handled => return handled(Path::Demand),
                Resolve::OutOfFrames => starved = true,
                Resolve::Refused => {}
            }
        }
        // The access was fine and the machine has no frame to back it: that
        // is not the program's bug, so no `SIGSEGV` (and no handler to run
        // on a stack that may itself need the frame). Linux's OOM killer
        // ends the process with `SIGKILL`; so do we.
        if starved && crate::arch::fault::from_user(saved_cs) {
            crate::mem::oom::kill_current();
        }
        // Still a fault: a ring-3 process with a `SIGSEGV` handler resumes
        // there (a kernel-mode fault must never have its frame rewritten to
        // jump into user code); everything else is contained below.
        if crate::arch::fault::from_user(saved_cs)
            && crate::task::signal::deliver_fault(
                rsp,
                crate::task::signal::FAULT_RIP_INDEX,
                fault.as_u64(),
                raw_error,
            )
        {
            return handled(Path::Signal);
        }
    }
    // SAFETY: `rsp` is the page-fault frame `page_fault_isr` saved, with RIP at
    // `FAULT_RIP_INDEX` (16) after the registers and the error code.
    unsafe {
        crate::arch::fault::contain_frame(
            rsp,
            crate::task::signal::FAULT_RIP_INDEX,
            Fault::BadAccess,
            format_args!("address {addr:?} ({error:?}), rip {rip:#x}"),
        )
    };
    // SAFETY: as above; RFLAGS is word 18, after CS.
    let rflags = unsafe { core::ptr::read_volatile((rsp + 18 * 8) as *const u64) };
    serial_println!(
        "EXCEPTION: page fault at {:?} ({:?}, raw {:#x}), rip {:#x}, cs {:#x}, rflags {:#x}, frame {:#x}",
        addr,
        error,
        raw_error,
        rip,
        saved_cs,
        rflags,
        rsp
    );
    if let Ok(fault) = addr {
        crate::arch::kernel_fault_report::print(rsp, fault.as_u64());
    }
    crate::halt();
}

/// Handler for one of the naked scheduler ISRs in `task::switch` (they save
/// the frame and switch tasks themselves).
fn naked_gate(isr: *const ()) -> x86_64::structures::idt::HandlerFunc {
    // SAFETY: callers pass only `timer_isr`/`yield_isr`, naked ISRs with a
    // compatible (no ABI) signature that end in `iretq`.
    unsafe { core::mem::transmute::<*const (), x86_64::structures::idt::HandlerFunc>(isr) }
}

extern "C" fn keyboard_handler(_stack: &InterruptStackFrameValue) {
    // Drain the i8042 (keyboard and auxiliary bytes alike, in order) and
    // decode what was collected, including bytes gathered while interrupts
    // were off (`input::ps2`).
    crate::perf::irq_entry();
    // Inside an interrupt window only collect: decoding takes locks the
    // interrupted code may hold (`arch::irq_window`).
    if super::irq_window::is_open() {
        crate::input::ps2::service();
    } else {
        crate::input::ps2::on_irq();
    }
    // Safety: we are in the IRQ1 handler.
    unsafe { super::irqchip::eoi(1) };
    crate::perf::irq_exit();
    // A key may have woken a task that should run now (P1.1).
    crate::task::preempt_point();
}

extern "C" fn mouse_handler(_stack: &InterruptStackFrameValue) {
    // The same intake as IRQ1: one FIFO keeps both ports' bytes in order.
    crate::perf::irq_entry();
    // Inside an interrupt window only collect: decoding takes locks the
    // interrupted code may hold (`arch::irq_window`).
    if super::irq_window::is_open() {
        crate::input::ps2::service();
    } else {
        crate::input::ps2::on_irq();
    }
    // Safety: we are in the IRQ12 handler.
    unsafe { super::irqchip::eoi(12) };
    crate::perf::irq_exit();
    crate::task::preempt_point();
}
