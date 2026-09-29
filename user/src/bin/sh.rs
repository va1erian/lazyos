//! `SH.ELF`: a small interactive interpreter running in ring 3.
//!
//! Dyon-inspired subset: `f64` numbers, booleans, strings, arrays, `let`,
//! `print`, `if`/`else`, arithmetic, comparisons, indexing and `&&`/`||`.
//!
//! The language and the command layer (`help`, `quit`, `cat`) live in the
//! shared `lazyos-lang` crate, so the desktop Terminal app (`xui-term`, a
//! `xuid` client) runs the same shell; this binary only supplies the console
//! I/O: the `write`/`read_char` syscalls and the `read_file` syscall for `cat`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::panic::PanicInfo;
use lazyos_lang::repl::{Flow, Shell, BANNER, CAT_LIMIT};
use user::sys;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let mut shell = Shell::new();
    sys::write_str(BANNER);

    let mut line = [0u8; 512];
    loop {
        sys::write_str("> ");
        let len = read_line(&mut line);
        let text = core::str::from_utf8(&line[..len]).unwrap_or("");
        let flow = shell.exec_line(text, &mut read_file, &mut |chunk| sys::write_str(chunk));
        if flow == Flow::Exit {
            sys::exit(0);
        }
    }
}

/// Read a line with basic backspace editing. Returns the byte length.
fn read_line(buffer: &mut [u8]) -> usize {
    let mut len = 0;
    loop {
        let ch = sys::read_char();
        if ch == b'\n' as u64 {
            sys::write_str("\n");
            return len;
        }
        if ch == 8 {
            if len > 0 {
                len -= 1;
                sys::write_str("\u{8} \u{8}");
            }
            continue;
        }
        if (32..127).contains(&ch) && len + 1 < buffer.len() {
            buffer[len] = ch as u8;
            len += 1;
            sys::write(&[ch as u8]);
        }
    }
}

/// `cat`'s file source: the `read_file` syscall, capped at [`CAT_LIMIT`].
fn read_file(name: &str) -> Option<Vec<u8>> {
    let mut name_z = [0u8; 64];
    let bytes = name.as_bytes();
    if bytes.len() >= name_z.len() {
        return None;
    }
    name_z[..bytes.len()].copy_from_slice(bytes);
    let mut buffer = [0u8; CAT_LIMIT];
    let count = sys::read_file(&name_z, &mut buffer)?;
    Some(buffer[..count].to_vec())
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
