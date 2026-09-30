//! The diagnostic a fatal ring-0 page fault prints before the kernel halts.
//!
//! A kernel `#PF` is symbolised from its `rip` alone only when the fault is
//! the instruction's own. Under a hypervisor that emulates an instruction (a
//! string port I/O such as `rep insw`), the fault can instead come from the
//! emulator's software page walk, so the report also prints the interrupted
//! registers and the raw paging-structure entries for `CR2` and the string
//! destination (`rdi`): the walk the emulator took, entry by entry. Every
//! line starts with `kernel:` so a log grep finds the report next to the
//! `EXCEPTION: page fault` line.

use super::pagewalk::{self, LEVELS};

/// Print the saved general registers and the page walks of `cr2` and `rdi`.
/// `frame` is the page-fault frame `page_fault_isr` saved (15 registers).
pub fn print(frame: u64, cr2: u64) {
    let word = |index: usize| {
        // SAFETY: `frame` is the saved page-fault frame; the 15 general
        // registers sit at words 0..15 (r15 first, rax last).
        unsafe { crate::task::sys::frame_word(frame, index) }
    };
    crate::serial_println!(
        "kernel: regs rax={:#x} rbx={:#x} rcx={:#x} rdx={:#x} rsi={:#x} rdi={:#x} rbp={:#x}",
        word(14),
        word(13),
        word(12),
        word(11),
        word(10),
        word(9),
        word(8)
    );
    print_walk("cr2", cr2);
    print_walk("rdi", word(9));
    crate::task::diag::print_kstack_report(frame);
}

/// Print each paging-structure entry mapping `addr` in the current address
/// space, down to the first not-present or leaf entry.
fn print_walk(label: &str, addr: u64) {
    let (entries, count) = pagewalk::walk(addr);
    for ((level, shift), entry) in LEVELS.iter().zip(entries).take(count) {
        let index = (addr >> shift) & 0x1FF;
        crate::serial_println!("kernel: walk {label} {addr:#x} {level}[{index}] = {entry:#x}");
    }
}
