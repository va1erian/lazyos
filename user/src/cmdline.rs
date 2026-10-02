//! Command lines for the `spawn` syscall ([`crate::sys::spawn`]), composed from
//! a program's `fhs::bin` path instead of a hand-written literal, so a program
//! that moves is renamed in one place (docs/filesystem-plan.md F3).

use alloc::vec::Vec;

/// The prefix that asks the kernel for the Linux ABI personality.
const LINUX_PREFIX: &str = "linux:";

/// `"<program> <args>\0"` for a native program (`args` may be empty).
pub fn native(program: &str, args: &str) -> Vec<u8> {
    compose("", program, args)
}

/// `"linux:<program> <args>\0"` for a static Linux-ABI program.
pub fn linux(program: &str, args: &str) -> Vec<u8> {
    compose(LINUX_PREFIX, program, args)
}

fn compose(prefix: &str, program: &str, args: &str) -> Vec<u8> {
    let mut line = Vec::with_capacity(prefix.len() + program.len() + args.len() + 2);
    line.extend_from_slice(prefix.as_bytes());
    line.extend_from_slice(program.as_bytes());
    if !args.is_empty() {
        line.push(b' ');
        line.extend_from_slice(args.as_bytes());
    }
    line.push(0);
    line
}
