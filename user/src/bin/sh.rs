//! A tiny line-based interpreter for LazyOS, running in ring 3.
//!
//! Grammar: `let x = <expr>`, `print <expr>`, `cat <file>`, `vars`, `help`,
//! `quit`, or a bare `<expr>`. Expressions support + - * / parentheses and
//! single-letter integer variables.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

const MAX_VARS: usize = 26;

struct Interp {
    vars: [i64; MAX_VARS],
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let mut interp = Interp {
        vars: [0; MAX_VARS],
    };
    let mut line = [0u8; 256];
    let mut len = 0usize;
    write_str("LazyOS tiny interpreter (ring 3)\n");
    write_str("Try: (1+2)*3   let x = 6*7   print x   cat HELLO.TXT   help\n");

    loop {
        write_str("> ");
        len = 0;
        loop {
            let ch = read_char();
            if ch == b'\n' as u64 {
                write_str("\n");
                break;
            }
            if ch == 8 {
                if len > 0 {
                    len -= 1;
                    write_str("\u{8} \u{8}");
                }
                continue;
            }
            if (32..127).contains(&ch) && len < line.len() - 1 {
                line[len] = ch as u8;
                len += 1;
                write_bytes(&[ch as u8]);
            }
        }
        let text = core::str::from_utf8(&line[..len]).unwrap_or("");
        interp.eval_line(text.trim());
    }
}

impl Interp {
    fn eval_line(&mut self, line: &str) {
        if line.is_empty() {
            return;
        }
        if line == "help" {
            write_str(
                "commands:\n  let x = <expr>\n  print <expr>\n  cat <file>\n  vars\n  quit\n",
            );
            return;
        }
        if line == "quit" || line == "exit" {
            sys_exit(0);
        }
        if line == "vars" {
            for (i, value) in self.vars.iter().enumerate() {
                if *value != 0 {
                    write_bytes(&[b'a' + i as u8]);
                    write_str(" = ");
                    print_int(*value);
                    write_str("\n");
                }
            }
            return;
        }
        if let Some(rest) = line.strip_prefix("let ") {
            if let Some((name, expr)) = rest.split_once('=') {
                let name = name.trim();
                if name.len() == 1 {
                    let ch = name.as_bytes()[0];
                    if ch.is_ascii_lowercase() {
                        if let Some(v) = self.eval(expr.trim()) {
                            self.vars[(ch - b'a') as usize] = v;
                            print_int(v);
                            write_str("\n");
                            return;
                        }
                    }
                }
            }
            write_str("syntax: let <a-z> = <expr>\n");
            return;
        }
        if let Some(rest) = line.strip_prefix("cat ") {
            self.cat(rest.trim());
            return;
        }
        if let Some(rest) = line.strip_prefix("print ") {
            match self.eval(rest) {
                Some(v) => {
                    print_int(v);
                    write_str("\n");
                }
                None => write_str("syntax error\n"),
            }
            return;
        }
        match self.eval(line) {
            Some(v) => {
                print_int(v);
                write_str("\n");
            }
            None => write_str("syntax error\n"),
        }
    }

    fn eval(&self, text: &str) -> Option<i64> {
        let mut parser = Parser {
            bytes: text.as_bytes(),
            pos: 0,
        };
        parser.skip();
        if parser.pos >= parser.bytes.len() {
            return None;
        }
        let value = parser.expr(&self.vars)?;
        parser.skip();
        if parser.pos == parser.bytes.len() {
            Some(value)
        } else {
            None
        }
    }

    /// `cat <file>`: demonstrates the `read_file` syscall.
    fn cat(&self, name: &str) {
        let mut name_buf = [0u8; 64];
        let bytes = name.as_bytes();
        if bytes.len() >= name_buf.len() {
            write_str("name too long\n");
            return;
        }
        name_buf[..bytes.len()].copy_from_slice(bytes);
        let mut buf = [0u8; 1024];
        let n = read_file(
            name_buf.as_ptr() as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        );
        if n == u64::MAX {
            write_str("not found: ");
            write_str(name);
            write_str("\n");
            return;
        }
        write_bytes(&buf[..n as usize]);
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn skip(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos] == b' ' {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip();
        self.bytes.get(self.pos).copied()
    }

    fn expr(&mut self, vars: &[i64; MAX_VARS]) -> Option<i64> {
        let mut value = self.term(vars)?;
        loop {
            match self.peek() {
                Some(b'+') => {
                    self.pos += 1;
                    value += self.term(vars)?;
                }
                Some(b'-') => {
                    self.pos += 1;
                    value -= self.term(vars)?;
                }
                _ => break,
            }
        }
        Some(value)
    }

    fn term(&mut self, vars: &[i64; MAX_VARS]) -> Option<i64> {
        let mut value = self.factor(vars)?;
        loop {
            match self.peek() {
                Some(b'*') => {
                    self.pos += 1;
                    value *= self.factor(vars)?;
                }
                Some(b'/') => {
                    self.pos += 1;
                    let divisor = self.factor(vars)?;
                    if divisor == 0 {
                        return None;
                    }
                    value /= divisor;
                }
                _ => break,
            }
        }
        Some(value)
    }

    fn factor(&mut self, vars: &[i64; MAX_VARS]) -> Option<i64> {
        match self.peek()? {
            b'(' => {
                self.pos += 1;
                let value = self.expr(vars)?;
                if self.peek() != Some(b')') {
                    return None;
                }
                self.pos += 1;
                Some(value)
            }
            b'-' => {
                self.pos += 1;
                Some(-self.factor(vars)?)
            }
            c if c.is_ascii_digit() => {
                let mut value = 0i64;
                while let Some(c) = self.peek() {
                    if c.is_ascii_digit() {
                        value = value * 10 + (c - b'0') as i64;
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                Some(value)
            }
            c if c.is_ascii_lowercase() => {
                self.pos += 1;
                Some(vars[(c - b'a') as usize])
            }
            _ => None,
        }
    }
}

fn print_int(value: i64) {
    let mut magnitude = if value < 0 {
        (-(value as i128)) as u128
    } else {
        value as u128
    };
    let mut buffer = [0u8; 20];
    let mut index = buffer.len();
    loop {
        index -= 1;
        buffer[index] = b'0' + (magnitude % 10) as u8;
        magnitude /= 10;
        if magnitude == 0 {
            break;
        }
    }
    if value < 0 {
        write_str("-");
    }
    write_bytes(&buffer[index..]);
}

fn write_str(text: &str) {
    write_bytes(text.as_bytes());
}

fn write_bytes(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
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

fn read_char() -> u64 {
    let ch: u64;
    unsafe {
        asm!("int 0x80", in("rax") 2u64, lateout("rax") ch, options(nostack));
    }
    ch
}

fn read_file(name: u64, buffer: u64, length: u64) -> u64 {
    let count: u64;
    unsafe {
        asm!(
            "int 0x80",
            in("rax") 3u64,
            in("rdi") name,
            in("rsi") buffer,
            in("rdx") length,
            lateout("rax") count,
            options(nostack),
        );
    }
    count
}

fn sys_exit(code: u32) -> ! {
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
