//! Interrupt descriptor table and handlers.

use crate::arch::pic;
use crate::input::keyboard;
use alloc::boxed::Box;
use core::sync::atomic::{AtomicU64, Ordering};
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
    idt.general_protection_fault
        .set_handler_fn(general_protection_fault_handler);
    idt.page_fault.set_handler_fn(page_fault_handler);
    idt.double_fault.set_handler_fn(double_fault_handler);
    // PIC IRQ0 (timer) and IRQ1 (keyboard) are remapped to vectors 32 and 33.
    idt[32].set_handler_fn(timer_handler);
    idt[33].set_handler_fn(keyboard_handler);

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

extern "x86-interrupt" fn general_protection_fault_handler(stack: InterruptStackFrame, error: u64) {
    serial_println!(
        "EXCEPTION: general protection fault (error {:#x})\n{:#?}",
        error,
        stack
    );
    crate::halt();
}

extern "x86-interrupt" fn page_fault_handler(
    stack: InterruptStackFrame,
    error: PageFaultErrorCode,
) {
    let addr = Cr2::read();
    serial_println!(
        "EXCEPTION: page fault at {:?} ({:?})\n{:#?}",
        addr,
        error,
        stack
    );
    crate::halt();
}

extern "x86-interrupt" fn double_fault_handler(stack: InterruptStackFrame, error: u64) -> ! {
    serial_println!("EXCEPTION: double fault (error {:#x})\n{:#?}", error, stack);
    crate::halt();
}

extern "x86-interrupt" fn timer_handler(_stack: InterruptStackFrame) {
    TICKS.fetch_add(1, Ordering::Relaxed);
    // Safety: we are in the IRQ0 handler.
    unsafe { pic::end_of_interrupt(0) };
}

extern "x86-interrupt" fn keyboard_handler(_stack: InterruptStackFrame) {
    // Drain the i8042 output buffer: rapid input can queue several scancodes
    // before one IRQ is serviced, and leaving them can overrun the controller.
    loop {
        // Safety: reading the i8042 status/output ports is valid in IRQ1.
        let status: u8 = unsafe { Port::<u8>::new(0x64).read() };
        if status & 0x01 == 0 {
            break;
        }
        let scancode: u8 = unsafe { Port::<u8>::new(0x60).read() };
        keyboard::push_scancode(scancode);
    }
    // Safety: we are in the IRQ1 handler.
    unsafe { pic::end_of_interrupt(1) };
}
