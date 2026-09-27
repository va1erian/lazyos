//! Interrupt descriptor table and handlers.

use crate::arch::pic;
use crate::input::{keyboard, mouse};
use alloc::boxed::Box;
use core::arch::global_asm;
use core::sync::atomic::AtomicU64;
use x86_64::instructions::port::Port;
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

/// Timer ticks since boot.
pub static TICKS: AtomicU64 = AtomicU64::new(0);

/// Build and load the IDT.
pub fn init() {
    let mut idt = InterruptDescriptorTable::new();
    idt.divide_error.set_handler_fn(divide_error_handler);
    idt.breakpoint.set_handler_fn(breakpoint_handler);
    idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
    idt.device_not_available
        .set_handler_fn(device_not_available_handler);
    idt.general_protection_fault
        .set_handler_fn(general_protection_fault_handler);
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
    // Safety: `page_fault_isr` is a naked handler with the (error-code-pushing)
    // layout #PF uses; it never returns a Rust value, only the frame pointer.
    unsafe {
        idt.page_fault
            .set_handler_addr(x86_64::VirtAddr::new(page_fault_isr as *const () as u64));
    }
    unsafe {
        idt.double_fault
            .set_handler_fn(double_fault_handler)
            .set_stack_index(crate::arch::gdt::DOUBLE_FAULT_IST);
    }
    // PIC IRQ0 (timer) drives preemption via the naked ISR in `task::switch`.
    idt[32].set_handler_fn(timer_gate());
    idt[33].set_handler_fn(keyboard_handler);
    idt[44].set_handler_fn(mouse_handler);
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

extern "x86-interrupt" fn divide_error_handler(stack: InterruptStackFrame) {
    serial_println!("EXCEPTION: divide error\n{:#?}", stack);
    crate::halt();
}

extern "x86-interrupt" fn breakpoint_handler(stack: InterruptStackFrame) {
    serial_println!("EXCEPTION: breakpoint\n{:#?}", stack);
}

extern "x86-interrupt" fn invalid_opcode_handler(stack: InterruptStackFrame) {
    serial_println!("EXCEPTION: invalid opcode\n{:#?}", stack);
    crate::halt();
}

extern "x86-interrupt" fn device_not_available_handler(stack: InterruptStackFrame) {
    serial_println!("EXCEPTION: device not available\n{:#?}", stack);
    crate::halt();
}

extern "x86-interrupt" fn stack_segment_fault_handler(stack: InterruptStackFrame, error: u64) {
    serial_println!(
        "EXCEPTION: stack segment fault (error {:#x})\n{:#?}",
        error,
        stack
    );
    crate::halt();
}

extern "x86-interrupt" fn segment_not_present_handler(stack: InterruptStackFrame, error: u64) {
    serial_println!(
        "EXCEPTION: segment not present (error {:#x})\n{:#?}",
        error,
        stack
    );
    crate::halt();
}

extern "x86-interrupt" fn invalid_tss_handler(stack: InterruptStackFrame, error: u64) {
    serial_println!("EXCEPTION: invalid TSS (error {:#x})\n{:#?}", error, stack);
    crate::halt();
}

extern "x86-interrupt" fn x87_floating_point_handler(stack: InterruptStackFrame) {
    serial_println!("EXCEPTION: x87 floating point\n{:#?}", stack);
    crate::halt();
}

extern "x86-interrupt" fn simd_floating_point_handler(stack: InterruptStackFrame) {
    serial_println!("EXCEPTION: SIMD floating point\n{:#?}", stack);
    crate::halt();
}

extern "x86-interrupt" fn alignment_check_handler(stack: InterruptStackFrame, error: u64) {
    serial_println!(
        "EXCEPTION: alignment check (error {:#x})\n{:#?}",
        error,
        stack
    );
    crate::halt();
}

extern "x86-interrupt" fn general_protection_fault_handler(stack: InterruptStackFrame, error: u64) {
    serial_println!(
        "EXCEPTION: general protection fault (error {:#x})\n{:#?}",
        error,
        stack
    );
    crate::halt();
}

// The page fault handler needs the interrupted general registers to build a
// `SIGSEGV` frame, and the `x86-interrupt` ABI does not expose them, so #PF
// uses a naked stub like the timer: push the registers, call into Rust with
// the frame pointer, resume at the (possibly rewritten) frame.
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
}

/// Resolve a page fault: COW copy, demand-zero page, or `SIGSEGV` into a
/// handler. Returns the (possibly rewritten) frame pointer to resume; without
/// a handler the diagnostic halt stays, exactly as before signals.
#[no_mangle]
extern "C" fn page_fault_dispatch(rsp: u64) -> u64 {
    // Frame: 15 general registers, then the error code, RIP, CS, RFLAGS, RSP, SS.
    let raw_error = unsafe { core::ptr::read_volatile((rsp + 15 * 8) as *const u64) };
    let error = PageFaultErrorCode::from_bits_truncate(raw_error);
    let addr = Cr2::read();
    if let Ok(fault) = addr {
        let table = crate::mem::kernel_table();
        // A write to a present copy-on-write user page takes a private copy.
        // This must come first: such a page is present, so the demand-zero
        // path below would never apply to it.
        if error.contains(PageFaultErrorCode::CAUSED_BY_WRITE)
            && crate::mem::cow_fault(table, fault.as_u64())
        {
            return rsp;
        }
        // A not-present page inside an Anon/Heap VMA is demand-zero memory:
        // materialize it (if the VMA permits this access) and resume.
        if crate::mem::demand_fault(table, fault.as_u64(), error) {
            return rsp;
        }
        // Still a fault: a process with a `SIGSEGV` handler resumes there;
        // everything else keeps the diagnostic halt.
        if crate::task::signal::deliver_fault(
            rsp,
            crate::task::signal::FAULT_RIP_INDEX,
            fault.as_u64(),
            raw_error,
        ) {
            return rsp;
        }
    }
    serial_println!(
        "EXCEPTION: page fault at {:?} ({:?}), frame {:#x}",
        addr,
        error,
        rsp
    );
    crate::halt();
}

extern "x86-interrupt" fn double_fault_handler(stack: InterruptStackFrame, error: u64) -> ! {
    serial_println!("EXCEPTION: double fault (error {:#x})\n{:#?}", error, stack);
    crate::halt();
}

/// Handler for the naked timer ISR (it switches tasks itself).
fn timer_gate() -> x86_64::structures::idt::HandlerFunc {
    // Safety: `timer_isr` is a naked ISR with a compatible (no ABI) signature.
    unsafe {
        core::mem::transmute::<*const (), x86_64::structures::idt::HandlerFunc>(
            crate::task::switch::timer_isr as *const (),
        )
    }
}

extern "x86-interrupt" fn keyboard_handler(_stack: InterruptStackFrame) {
    // Drain the i8042 output buffer, but only keyboard bytes (aux bit clear).
    loop {
        // Safety: reading the i8042 status/output ports is valid in IRQ1.
        let status: u8 = unsafe { Port::<u8>::new(0x64).read() };
        if status & 0x01 == 0 || status & 0x20 != 0 {
            break;
        }
        let scancode: u8 = unsafe { Port::<u8>::new(0x60).read() };
        keyboard::push_scancode(scancode);
    }
    // Safety: we are in the IRQ1 handler.
    unsafe { pic::end_of_interrupt(1) };
}

extern "x86-interrupt" fn mouse_handler(_stack: InterruptStackFrame) {
    // Read every pending byte that came from the auxiliary device.
    loop {
        // Safety: reading the i8042 status/output ports is valid in IRQ12.
        let status: u8 = unsafe { Port::<u8>::new(0x64).read() };
        if status & 0x01 == 0 || status & 0x20 == 0 {
            break;
        }
        let byte: u8 = unsafe { Port::<u8>::new(0x60).read() };
        mouse::push_byte(byte);
    }
    // Safety: we are in the IRQ12 handler.
    unsafe { pic::end_of_interrupt(12) };
}
