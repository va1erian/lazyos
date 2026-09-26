//! `SH.ELF`: a small interactive interpreter running in ring 3.
//!
//! Dyon-inspired subset: `f64` numbers, booleans, strings, arrays, `let`,
//! `print`, `if`/`else`, arithmetic, comparisons, indexing and `&&`/`||`.
//!
//! Modules: `lexer` (tokenizer), `parser` (AST), `interp` (evaluator),
//! `value` (runtime values).

#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;
use user::lang::{interp, lexer, parser};
use user::sys;

const BANNER: &str = "LazyOS interpreter (ring 3)\n\
    Try: [1,2,3]   let x = 6*7   x*2   \"hi\" + \" there\"   cat HELLO.TXT\n";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let mut interpreter = interp::Interp::new();
    sys::write_str(BANNER);

    let mut line = [0u8; 512];
    loop {
        sys::write_str("> ");
        let len = read_line(&mut line);
        let text = core::str::from_utf8(&line[..len]).unwrap_or("").trim();
        if text.is_empty() {
            continue;
        }
        match text {
            "quit" | "exit" => sys::exit(0),
            "help" => sys::write_str(BANNER),
            _ if text.starts_with("cat ") => cat(text[4..].trim()),
            _ => match lexer::lex(text).and_then(parser::parse) {
                Ok(stmts) => {
                    if let Err(message) = interpreter.run(&stmts) {
                        report(&message);
                    }
                }
                Err(message) => report(&message),
            },
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

/// `cat <file>`: demonstrate the `read_file` syscall.
fn cat(name: &str) {
    let mut name_z = [0u8; 64];
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() >= name_z.len() {
        report("usage: cat <file>");
        return;
    }
    name_z[..bytes.len()].copy_from_slice(bytes);

    let mut buffer = [0u8; 1024];
    match sys::read_file(&name_z, &mut buffer) {
        Some(n) => sys::write(&buffer[..n]),
        None => report("file not found"),
    }
}

fn report(message: &str) {
    sys::write_str("error: ");
    sys::write_str(message);
    sys::write_str("\n");
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
