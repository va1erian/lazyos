//! Example LazyOS ring-3 program: greet, read a line, exit.
//!
//! Uses the shared runtime in `user` (`sys` + heap allocator).

#![no_std]
#![no_main]

use core::panic::PanicInfo;
use user::sys;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str(
        "Hello from ring 3!\n\
         This program was loaded from the FAT16 disk by the LazyOS ELF loader\n\
         and is running in user mode (CPL=3).\n\n\
         Type a line and press Enter: ",
    );
    loop {
        let ch = sys::read_char();
        if ch == b'\n' as u64 {
            break;
        }
        if (32..127).contains(&ch) {
            sys::write(&[ch as u8]);
        }
    }
    sys::write_str("\nGoodbye from user mode!\n");
    sys::exit(0)
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
