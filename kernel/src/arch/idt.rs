//! Interrupt descriptor table and handlers.

use crate::arch::fault::{contain, Fault};
use crate::arch::fault_storm::Path;
use crate::arch::io::inb;
use crate::arch::pic;
use crate::input::{keyboard, mouse};
use crate::task::signal::Exception;
use alloc::boxed::Box;
use core::arch::global_asm;
use core::sync::atomic::AtomicU64;
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

/// Timer ticks since boot.
pub static TICKS: AtomicU64 = AtomicU64::new(0);

/// Build and load the IDT.
pub fn init() {
    let mut idt = InterruptDescriptorTable::new();
    idt.breakpoint.set_handler_fn(breakpoint_handler);
    // Issue #382: an injected NMI prints a hang report, even over IF=0 spins.
    idt.non_maskable_interrupt
        .set_handler_fn(super::nmi::nmi_handler);
    idt.device_not_available
        .set_handler_fn(device_not_available_handler);
    idt.stack_segment_fault
        .set_handler_fn(stack_segment_fault_handler);
    idt.segment_not_present
        .set_handler_fn(segment_not_present_handler);
    idt.invalid_tss.set_handler_fn(invalid_tss_handler);
    idt.x87_floating_point
        .set_handler_fn(x87_floating_point_handler);
    idt.simd_floating_point
        .set_handler_fn(simd_floating_point_handler);
    idt.alignment_check.set_handler_fn(alignment_check_handler);
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
    // Safety: `DOUBLE_FAULT_IST` names a valid TSS interrupt-stack-table slot
    // that `gdt::init` already set up with its own dedicated stack.
    unsafe {
        idt.double_fault
            .set_handler_fn(double_fault_handler)
            .set_stack_index(crate::arch::gdt::DOUBLE_FAULT_IST);
    }
    // PIC IRQ0 (timer) drives preemption via the naked ISR in `task::switch`.
    idt[32].set_handler_fn(naked_gate(crate::task::switch::timer_isr as *const ()));
    // Voluntary reschedules (a task parking) use their own vector so they
    // never count as a PIT tick or send a spurious EOI (issue #338). DPL 0:
    // ring 3 cannot raise it.
    idt[crate::task::switch::YIELD_VECTOR]
        .set_handler_fn(naked_gate(crate::task::switch::yield_isr as *const ()));
    idt[33].set_handler_fn(keyboard_handler);
    idt[44].set_handler_fn(mouse_handler);
    // Every other PIC line reaches the device core (issue #240).
    super::irq_stubs::install(&mut idt);
    // int 0x80: user-mode syscall gate (DPL 3).
    idt[0x80]
        .set_handler_fn(crate::process::syscall_gate())
        .set_privilege_level(x86_64::PrivilegeLevel::Ring3);

    // Leak so the table outlives this call; `load` needs a 'static reference.
    let idt: &'static InterruptDescriptorTable = Box::leak(Box::new(idt));
    idt.load();
}

/// Initialise interrupt hardware (PIC + PIT) and load the IDT.
pub fn init_hardware() {
    // Safety: reprogramming the PIC/PIT is only valid once and before enabling IRQs.
    unsafe {
        pic::init();
        pic::init_pit(100);
    }
    init();
}

/// A fault from ring 3 ends only the faulting process and never returns; a
/// ring-0 fault falls through to the caller's diagnostic halt (issue #7).
fn user_fault(stack: &InterruptStackFrame, fault: Fault) {
    contain(
        stack.code_segment.0 as u64,
        fault,
        format_args!("rip {:#x}", stack.instruction_pointer.as_u64()),
    );
}

extern "x86-interrupt" fn breakpoint_handler(stack: InterruptStackFrame) {
    serial_println!("EXCEPTION: breakpoint\n{:#?}", stack);
}

extern "x86-interrupt" fn device_not_available_handler(stack: InterruptStackFrame) {
    user_fault(&stack, Fault::DeviceNotAvailable);
    serial_println!("EXCEPTION: device not available\n{:#?}", stack);
    crate::halt();
}

extern "x86-interrupt" fn stack_segment_fault_handler(stack: InterruptStackFrame, error: u64) {
    user_fault(&stack, Fault::StackSegment);
    serial_println!(
        "EXCEPTION: stack segment fault (error {:#x})\n{:#?}",
        error,
        stack
    );
    crate::halt();
}

extern "x86-interrupt" fn segment_not_present_handler(stack: InterruptStackFrame, error: u64) {
    user_fault(&stack, Fault::SegmentNotPresent);
    serial_println!(
        "EXCEPTION: segment not present (error {:#x})\n{:#?}",
        error,
        stack
    );
    crate::halt();
}

extern "x86-interrupt" fn invalid_tss_handler(stack: InterruptStackFrame, error: u64) {
    user_fault(&stack, Fault::InvalidTss);
    serial_println!("EXCEPTION: invalid TSS (error {:#x})\n{:#?}", error, stack);
    crate::halt();
}

extern "x86-interrupt" fn x87_floating_point_handler(stack: InterruptStackFrame) {
    user_fault(&stack, Fault::FloatingPoint);
    serial_println!("EXCEPTION: x87 floating point\n{:#?}", stack);
    crate::halt();
}

extern "x86-interrupt" fn simd_floating_point_handler(stack: InterruptStackFrame) {
    user_fault(&stack, Fault::FloatingPoint);
    serial_println!("EXCEPTION: SIMD floating point\n{:#?}", stack);
    crate::halt();
}

extern "x86-interrupt" fn alignment_check_handler(stack: InterruptStackFrame, error: u64) {
    user_fault(&stack, Fault::AlignmentCheck);
    serial_println!(
        "EXCEPTION: alignment check (error {:#x})\n{:#?}",
        error,
        stack
    );
    crate::halt();
}

// The page fault handler needs the interrupted general registers to build a
// `SIGSEGV` frame, and the `x86-interrupt` ABI does not expose them, so #PF
// (and #DE/#UD/#GP, issue #246) use naked stubs like the timer: push the
// registers, call into Rust with the frame pointer, resume at the (possibly
// rewritten) frame.
global_asm!(
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
        iretq
    .endm

    exception_isr divide_error_isr, 0, 0
    exception_isr invalid_opcode_isr, 6, 0
    exception_isr general_protection_isr, 13, 1
    "#
);

global_asm!(
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
        iretq
    "#
);

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
        // A write to a present copy-on-write user page takes a private copy.
        // This must come first: such a page is present, so the demand-zero
        // path below would never apply to it.
        if error.contains(PageFaultErrorCode::CAUSED_BY_WRITE)
            && crate::mem::cow_fault(table, fault.as_u64())
        {
            return handled(Path::Cow);
        }
        // A not-present page inside an Anon/Heap VMA is demand-zero memory:
        // materialize it (if the VMA permits this access) and resume.
        if crate::mem::demand_fault(table, fault.as_u64(), error) {
            return handled(Path::Demand);
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

extern "x86-interrupt" fn double_fault_handler(stack: InterruptStackFrame, error: u64) -> ! {
    serial_println!("EXCEPTION: double fault (error {:#x})\n{:#?}", error, stack);
    crate::halt();
}

/// Handler for one of the naked scheduler ISRs in `task::switch` (they save
/// the frame and switch tasks themselves).
fn naked_gate(isr: *const ()) -> x86_64::structures::idt::HandlerFunc {
    // SAFETY: callers pass only `timer_isr`/`yield_isr`, naked ISRs with a
    // compatible (no ABI) signature that end in `iretq`.
    unsafe { core::mem::transmute::<*const (), x86_64::structures::idt::HandlerFunc>(isr) }
}

extern "x86-interrupt" fn keyboard_handler(_stack: InterruptStackFrame) {
    // Drain the i8042 output buffer, but only keyboard bytes (aux bit clear).
    loop {
        // Safety: reading the i8042 status/output ports is valid in IRQ1.
        let status: u8 = unsafe { inb(0x64) };
        if status & 0x01 == 0 || status & 0x20 != 0 {
            break;
        }
        // Safety: `status` just confirmed the output buffer holds a byte.
        let scancode: u8 = unsafe { inb(0x60) };
        keyboard::push_scancode(scancode);
    }
    // Safety: we are in the IRQ1 handler.
    unsafe { pic::end_of_interrupt(1) };
}

extern "x86-interrupt" fn mouse_handler(_stack: InterruptStackFrame) {
    // Read every pending byte that came from the auxiliary device.
    loop {
        // Safety: reading the i8042 status/output ports is valid in IRQ12.
        let status: u8 = unsafe { inb(0x64) };
        if status & 0x01 == 0 || status & 0x20 == 0 {
            break;
        }
        // Safety: `status` just confirmed the output buffer holds a byte.
        let byte: u8 = unsafe { inb(0x60) };
        mouse::push_byte(byte);
    }
    // Safety: we are in the IRQ12 handler.
    unsafe { pic::end_of_interrupt(12) };
}
