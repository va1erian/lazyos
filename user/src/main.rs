//! Example LazyOS ring-3 program.
//!
//! Built as a static ELF64 at 0x400000 and run by the kernel's ELF loader.
//! Talks to the kernel through `int 0x80` syscalls.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys_write(
        b"Hello from ring 3!\n\
          This program was loaded from the FAT16 disk by the LazyOS ELF loader\n\
          and is running in user mode (CPL=3).\n",
    );
    sys_write(b"\nType a line and press Enter: ");
    loop {
        let ch = sys_read_char();
        if ch == b'\n' as u64 {
            break;
        }
        if (32..127).contains(&ch) {
            put_byte(ch as u8);
        }
    }
    sys_write(b"\nGoodbye from user mode!\n");
    sys_exit(0)
}

fn sys_write(bytes: &[u8]) {
    // Safety: `int 0x80` with syscall 1 (write) and a valid buffer.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") 1u64,
            in("rdi") bytes.as_ptr() as u64,
            in("rsi") bytes.len() as u64,
            lateout("rax") _,
            options(nostack),
        );
    }
}

fn put_byte(byte: u8) {
    // Safety: `int 0x80` with syscall 1 (write), one byte.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") 1u64,
            in("rdi") &byte as *const u8 as u64,
            in("rsi") 1u64,
            lateout("rax") _,
            options(nostack),
        );
    }
}

fn sys_read_char() -> u64 {
    let ch: u64;
    // Safety: `int 0x80` with syscall 2 (read char); result returned in rax.
    unsafe {
        asm!("int 0x80", in("rax") 2u64, lateout("rax") ch, options(nostack));
    }
    ch
}

fn sys_exit(code: u32) -> ! {
    // Safety: `int 0x80` with syscall 0 (exit); does not return to this program.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") 0u64,
            in("rdi") code as u64,
            options(noreturn, nostack),
        );
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys_exit(1)
}
