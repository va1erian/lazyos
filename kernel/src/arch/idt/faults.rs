//! The exceptions with no register-saving stub of their own (`#BP`, `#NM`,
//! `#SS`, `#NP`, `#TS`, `#MF`, `#XM`, `#AC`, `#DF`): `extern "C"` handlers
//! behind `arch::gsgate`'s paranoid gates (an exception may land inside a
//! `swapgs` window, so the gate reads `IA32_GS_BASE` rather than trusting
//! `CS`).

use crate::arch::fault::{contain, Fault};
use crate::arch::gsgate::paranoid_gate;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrameValue};
use x86_64::VirtAddr;

paranoid_gate!(breakpoint_gate => breakpoint_handler);
paranoid_gate!(device_not_available_gate => device_not_available_handler);
paranoid_gate!(stack_segment_gate => stack_segment_fault_handler, err);
paranoid_gate!(segment_not_present_gate => segment_not_present_handler, err);
paranoid_gate!(invalid_tss_gate => invalid_tss_handler, err);
paranoid_gate!(x87_floating_point_gate => x87_floating_point_handler);
paranoid_gate!(simd_floating_point_gate => simd_floating_point_handler);
paranoid_gate!(alignment_check_gate => alignment_check_handler, err);
paranoid_gate!(double_fault_gate => double_fault_handler, err, unlocked);

/// The IDT address of a gate.
fn addr(gate: unsafe extern "C" fn()) -> VirtAddr {
    VirtAddr::new(gate as *const () as u64)
}

/// Install every gate of this file.
pub(super) fn install(idt: &mut InterruptDescriptorTable) {
    // SAFETY: each gate is a naked stub with its vector's frame layout (an
    // error code exactly for #SS, #NP, #TS, #AC and #DF) that ends in
    // `iretq` (or never returns, #DF); `DOUBLE_FAULT_IST` names the TSS
    // slot `gdt::init_cpu` gave its own stack.
    unsafe {
        idt.breakpoint.set_handler_addr(addr(breakpoint_gate));
        idt.device_not_available
            .set_handler_addr(addr(device_not_available_gate));
        idt.stack_segment_fault
            .set_handler_addr(addr(stack_segment_gate));
        idt.segment_not_present
            .set_handler_addr(addr(segment_not_present_gate));
        idt.invalid_tss.set_handler_addr(addr(invalid_tss_gate));
        idt.x87_floating_point
            .set_handler_addr(addr(x87_floating_point_gate));
        idt.simd_floating_point
            .set_handler_addr(addr(simd_floating_point_gate));
        idt.alignment_check
            .set_handler_addr(addr(alignment_check_gate));
        idt.double_fault
            .set_handler_addr(addr(double_fault_gate))
            .set_stack_index(crate::arch::gdt::DOUBLE_FAULT_IST);
    }
}

/// A fault from ring 3 ends only the faulting process and never returns; a
/// ring-0 fault falls through to the caller's diagnostic halt (issue #7).
fn user_fault(stack: &InterruptStackFrameValue, fault: Fault) {
    contain(
        stack.code_segment.0 as u64,
        fault,
        format_args!("rip {:#x}", stack.instruction_pointer.as_u64()),
    );
}

extern "C" fn breakpoint_handler(stack: &InterruptStackFrameValue) {
    serial_println!("EXCEPTION: breakpoint\n{:#?}", stack);
}

extern "C" fn device_not_available_handler(stack: &InterruptStackFrameValue) {
    user_fault(stack, Fault::DeviceNotAvailable);
    serial_println!("EXCEPTION: device not available\n{:#?}", stack);
    crate::halt();
}

extern "C" fn stack_segment_fault_handler(stack: &InterruptStackFrameValue, error: u64) {
    user_fault(stack, Fault::StackSegment);
    serial_println!(
        "EXCEPTION: stack segment fault (error {:#x})\n{:#?}",
        error,
        stack
    );
    crate::halt();
}

extern "C" fn segment_not_present_handler(stack: &InterruptStackFrameValue, error: u64) {
    user_fault(stack, Fault::SegmentNotPresent);
    serial_println!(
        "EXCEPTION: segment not present (error {:#x})\n{:#?}",
        error,
        stack
    );
    crate::halt();
}

extern "C" fn invalid_tss_handler(stack: &InterruptStackFrameValue, error: u64) {
    user_fault(stack, Fault::InvalidTss);
    serial_println!("EXCEPTION: invalid TSS (error {:#x})\n{:#?}", error, stack);
    crate::halt();
}

extern "C" fn x87_floating_point_handler(stack: &InterruptStackFrameValue) {
    user_fault(stack, Fault::FloatingPoint);
    serial_println!("EXCEPTION: x87 floating point\n{:#?}", stack);
    crate::halt();
}

extern "C" fn simd_floating_point_handler(stack: &InterruptStackFrameValue) {
    user_fault(stack, Fault::FloatingPoint);
    serial_println!("EXCEPTION: SIMD floating point\n{:#?}", stack);
    crate::halt();
}

extern "C" fn alignment_check_handler(stack: &InterruptStackFrameValue, error: u64) {
    user_fault(stack, Fault::AlignmentCheck);
    serial_println!(
        "EXCEPTION: alignment check (error {:#x})\n{:#?}",
        error,
        stack
    );
    crate::halt();
}

extern "C" fn double_fault_handler(stack: &InterruptStackFrameValue, error: u64) -> ! {
    serial_println!("EXCEPTION: double fault (error {:#x})\n{:#?}", error, stack);
    crate::halt();
}
