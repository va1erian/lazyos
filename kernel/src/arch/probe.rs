//! Fault-recovering memory probes (test kernels only): one read and one
//! write instruction with a page-fault fixup, like a Linux exception-table
//! entry. A ring-0 `#PF` at either site resumes at the fixup, which reports
//! the fault instead of halting.
//!
//! The SMP suite uses them where only a fault can tell: whether an AP still
//! holds a writable translation after `mprotect` tightened it, and to take
//! an exception (whose `iretq` unblocks NMIs) inside the NMI handler.

use core::arch::global_asm;

use crate::task::signal::FAULT_RIP_INDEX;

// `lazyos_probe_read(addr: rdi) -> rax`: 0, or 1 when the read faulted (the
// value read is discarded). `lazyos_probe_write(addr: rdi, value: rsi) ->
// rax`: the same for a store. Nothing is pushed between a site and its
// `ret`, so the fixup returns with the frame the site faulted on.
global_asm!(
    r#"
    .global lazyos_probe_read
    .global lazyos_probe_read_site
    .global lazyos_probe_write
    .global lazyos_probe_write_site
    .global lazyos_probe_fixup
lazyos_probe_read:
    xor eax, eax
lazyos_probe_read_site:
    mov rdx, qword ptr [rdi]
    ret
lazyos_probe_write:
    xor eax, eax
lazyos_probe_write_site:
    mov qword ptr [rdi], rsi
    ret
lazyos_probe_fixup:
    mov eax, 1
    ret
"#
);

extern "C" {
    fn lazyos_probe_read(addr: u64) -> u64;
    fn lazyos_probe_read_site();
    fn lazyos_probe_write(addr: u64, value: u64) -> u64;
    fn lazyos_probe_write_site();
    fn lazyos_probe_fixup();
}

/// Read the word at `addr`; whether the read faulted.
pub fn read_faults(addr: u64) -> bool {
    // SAFETY: the routine reads one word and any fault at its site resumes
    // at the fixup (`recover`), so no address can take the kernel down.
    unsafe { lazyos_probe_read(addr) != 0 }
}

/// Store `value` at `addr`; whether the store faulted. A store that does
/// not fault lands: only point it at memory the caller owns.
pub fn write_faults(addr: u64, value: u64) -> bool {
    // SAFETY: as `read_faults`; the caller owns the word (when it does not
    // fault) by this function's contract.
    unsafe { lazyos_probe_write(addr, value) != 0 }
}

/// The page-fault path's hook: a ring-0 fault at a probe site resumes at
/// the fixup. `frame` is the frame `page_fault_isr` saved.
pub fn recover(frame: u64) -> bool {
    // SAFETY: `frame` is the saved page-fault frame; RIP and CS sit at
    // fixed words of it.
    let (rip, cs) = unsafe {
        (
            crate::task::sys::frame_word(frame, FAULT_RIP_INDEX),
            crate::task::sys::frame_word(frame, FAULT_RIP_INDEX + 1),
        )
    };
    let sites = [
        lazyos_probe_read_site as *const () as u64,
        lazyos_probe_write_site as *const () as u64,
    ];
    if crate::arch::fault::from_user(cs) || !sites.contains(&rip) {
        return false;
    }
    // SAFETY: as above; the fixup is in this module and needs no other
    // register than the ones the site left.
    unsafe {
        crate::task::sys::put_frame_word(
            frame,
            FAULT_RIP_INDEX,
            lazyos_probe_fixup as *const () as u64,
        )
    };
    true
}
