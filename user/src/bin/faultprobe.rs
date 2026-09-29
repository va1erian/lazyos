//! `faultprobe` (`FAULTPRB.ELF`): a ring-3 program that misbehaves on purpose
//! (issue #7), to prove the kernel contains user faults.
//!
//! Run it from the shell as `exec FAULTPRB.ELF <mode>`. Each mode provokes one
//! exception the kernel must turn into the death of *this* process only, with
//! the `128 + signal` exit status; the shell then prints the verdict and its
//! prompt comes back:
//!
//! | mode | what it does | exception | signal |
//! |------|--------------|-----------|--------|
//! | `null` | reads address 0 | #PF | SIGSEGV (11) |
//! | `kernel` | writes a kernel address | #PF | SIGSEGV (11) |
//! | `priv` | executes `cli` in ring 3 | #GP | SIGSEGV (11) |
//! | `div` | divides by zero | #DE | SIGFPE (8) |
//! | `ud` | executes `ud2` | #UD | SIGILL (4) |
//!
//! With no mode (or an unknown one) it exits 0 without faulting.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;
use user::sys;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let mut args = [0u8; 32];
    let len = sys::service_args(&mut args).min(args.len());
    let mode = core::str::from_utf8(&args[..len]).unwrap_or("").trim();
    sys::write_str("faultprobe: mode ");
    sys::write_str(mode);
    sys::write_str("\n");
    match mode {
        "null" => null_read(),
        "kernel" => kernel_write(),
        "priv" => privileged(),
        "div" => divide_by_zero(),
        "ud" => undefined(),
        _ => {}
    }
    sys::write_str("faultprobe: no fault requested\n");
    sys::exit(0)
}

fn null_read() {
    // SAFETY: deliberately faults; address 0 is never mapped in user space.
    let value = unsafe { core::ptr::read_volatile(core::ptr::null::<u64>()) };
    core::hint::black_box(value);
}

fn kernel_write() {
    // SAFETY: deliberately faults; the kernel half is supervisor-only, so a
    // ring-3 write must be refused by the CPU.
    unsafe { core::ptr::write_volatile(0xffff_8000_0000_0000 as *mut u64, 1) };
}

fn privileged() {
    // SAFETY: deliberately faults; `cli` is a privileged instruction (#GP in
    // ring 3), so the CPU refuses it before it has any effect.
    unsafe { asm!("cli", options(nomem, nostack)) };
}

fn divide_by_zero() {
    let zero = core::hint::black_box(0u64);
    let quotient: u64;
    // SAFETY: deliberately faults with #DE; `div` reads and writes only the
    // named registers.
    unsafe {
        asm!("div {0}", in(reg) zero, inout("rax") 1u64 => quotient, out("rdx") _, options(nomem, nostack));
    }
    core::hint::black_box(quotient);
}

fn undefined() {
    // SAFETY: deliberately faults; `ud2` is the architecturally undefined opcode.
    unsafe { asm!("ud2", options(nomem, nostack)) };
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
