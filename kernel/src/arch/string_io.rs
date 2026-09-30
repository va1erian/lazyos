//! `rep insw` with a fault fixup: the one kernel instruction a hypervisor
//! must emulate with a software page walk.
//!
//! A string port instruction exits to the hypervisor, which emulates the
//! whole run: it translates the destination through the guest page tables
//! itself, then stores the port data there. Under WHPX that translation
//! occasionally fails for a destination that is mapped and writable, and the
//! emulator injects a fabricated `#PF`: an error code no CPU can produce (the
//! reserved-bit flag without the present flag) and `CR2` holding whatever was
//! in `rax`. Before this fixup, init's ELF load died on it in ring 0 about one
//! desktop boot in several hundred under host load.
//!
//! So the instruction carries a fixup, like a Linux exception-table entry: a
//! ring-0 fault at [`site`] whose remaining destination is mapped writable
//! cannot be the instruction's own, and [`recover`] resumes at the fixup
//! instead, which reports the transfer as aborted. Whether the emulator
//! consumed port data before faulting is unknowable, so the caller must
//! restart the device operation rather than resume the copy. A fault whose
//! destination really is unmapped stays fatal: that is a kernel bug.

use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::task::signal::FAULT_RIP_INDEX;

/// Frame words of the page-fault frame `page_fault_isr` saves.
const RDI: usize = 9;
const RCX: usize = 12;
const CS: usize = FAULT_RIP_INDEX + 1;

/// Spurious faults recovered since boot; the first few are also logged.
static RECOVERED: AtomicU64 = AtomicU64::new(0);
const LOGGED: u64 = 4;

// `lazyos_insw(port: dx <- edi, dst: rdi <- rsi, words: rcx <- rdx) -> rax`:
// 0 when every word arrived, 1 when a fault was recovered at the fixup. The
// routine touches no stack between the site and `ret`, so the fixup returns
// with the frame the site faulted on.
global_asm!(
    r#"
    .global lazyos_insw
    lazyos_insw:
        mov rcx, rdx
        mov edx, edi
        mov rdi, rsi
    .global lazyos_insw_site
    lazyos_insw_site:
        rep insw
        xor eax, eax
        ret
    .global lazyos_insw_fixup
    lazyos_insw_fixup:
        mov eax, 1
        ret
    "#
);

extern "C" {
    fn lazyos_insw(port: u32, dst: *mut u8, words: u64) -> u64;
    fn lazyos_insw_site();
    fn lazyos_insw_fixup();
}

/// A transfer the hypervisor's emulator aborted with a spurious fault. The
/// device may have lost data mid-sector: restart the operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aborted;

/// Read `buf.len() / 2` words from `port` into `buf` with one `rep insw`.
///
/// # Safety
/// Same contract as [`super::io::inb`], for every word read. `buf.len()` must
/// be even.
pub unsafe fn insw(port: u16, buf: &mut [u8]) -> Result<(), Aborted> {
    debug_assert!(buf.len().is_multiple_of(2));
    #[cfg(lazyos_tests)]
    if harness::take_injection() {
        // SAFETY: forwarded from the caller's contract.
        return unsafe { harness::inject(port, buf.len() as u64 / 2) };
    }
    // SAFETY: `buf` is the caller's exclusive slice and `lazyos_insw` writes
    // at most `buf.len() / 2` words into it (fewer when it aborts). The
    // direction flag is clear on entry (the SysV ABI guarantees it).
    match unsafe { lazyos_insw(u32::from(port), buf.as_mut_ptr(), buf.len() as u64 / 2) } {
        0 => Ok(()),
        _ => Err(Aborted),
    }
}

/// The address of the `rep insw` instruction.
pub fn site() -> u64 {
    lazyos_insw_site as *const () as u64
}

/// Resume a ring-0 fault at [`site`] at the fixup when the fault cannot be
/// the instruction's own: its remaining destination (`rdi`, `rcx` words) is
/// mapped writable. Returns false, leaving the frame alone, for any other
/// fault. `frame` is the page-fault frame `page_fault_isr` saved.
pub fn recover(frame: u64) -> bool {
    let word = |index: usize| {
        // SAFETY: `frame` is the saved page-fault frame; RDI, RCX, RIP and CS
        // sit at fixed words of it.
        unsafe { crate::task::sys::frame_word(frame, index) }
    };
    if word(FAULT_RIP_INDEX) != site() || crate::arch::fault::from_user(word(CS)) {
        return false;
    }
    let (dst, words) = (word(RDI), word(RCX));
    if !destination_writable(dst, words) && !harness::forced() {
        return false;
    }
    let count = RECOVERED.fetch_add(1, Ordering::Relaxed) + 1;
    if count <= LOGGED {
        crate::serial_println!(
            "kernel: spurious #PF at rep insw (dst {dst:#x}, {words} words left): \
             hypervisor emulation fault, transfer aborted ({count} so far)"
        );
    }
    // SAFETY: `frame` is the saved page-fault frame; rewriting its RIP makes
    // `iretq` resume at the fixup, on the same stack the site ran on.
    unsafe {
        crate::task::sys::put_frame_word(
            frame,
            FAULT_RIP_INDEX,
            lazyos_insw_fixup as *const () as u64,
        )
    };
    true
}

/// Spurious faults recovered since boot.
#[cfg(lazyos_tests)]
pub fn recovered() -> u64 {
    RECOVERED.load(Ordering::Relaxed)
}

/// Whether every byte of the `words`-word destination at `dst` is mapped
/// writable in the current address space. A zero or wrapping range is not.
fn destination_writable(dst: u64, words: u64) -> bool {
    let Some(last) = words
        .checked_mul(2)
        .filter(|bytes| *bytes != 0)
        .and_then(|bytes| dst.checked_add(bytes - 1))
    else {
        return false;
    };
    let mut page = dst & !0xFFF;
    loop {
        if !super::pagewalk::writable(page) {
            return false;
        }
        if page >= (last & !0xFFF) {
            return true;
        }
        page += 0x1000;
    }
}

/// Test hooks. A hypervisor's spurious fault cannot be summoned on demand, so
/// tests stand one in with a genuine `#PF`: `rep insw` into an unmapped
/// address while [`force`] makes [`recover`] accept it. That drives the real
/// ISR, dispatch, fixup and `iretq` path. Under TCG the port read happens
/// before the faulting store, so an injected abort also loses a data word,
/// the worst case a driver's restart has to survive.
pub mod harness {
    use core::sync::atomic::{AtomicBool, Ordering};

    static FORCE: AtomicBool = AtomicBool::new(false);

    pub(super) fn forced() -> bool {
        cfg!(lazyos_tests) && FORCE.load(Ordering::Relaxed)
    }

    #[cfg(lazyos_tests)]
    pub use tests_only::*;

    #[cfg(lazyos_tests)]
    mod tests_only {
        use super::{Ordering, FORCE};
        use core::sync::atomic::AtomicU64;

        /// Top page of the lower half: user space, never mapped in the kernel
        /// task's address space the test suite runs in.
        pub const UNMAPPED: u64 = 0x0000_7FFF_FFFF_E000;

        static SKIP: AtomicU64 = AtomicU64::new(0);
        static INJECT: AtomicU64 = AtomicU64::new(0);

        /// Accept (or stop accepting) faults with an unmapped destination.
        pub fn force(on: bool) {
            FORCE.store(on, Ordering::Relaxed);
        }

        /// Let the next `skip` [`super::super::insw`] calls through, then
        /// abort each of the `count` after them.
        pub fn inject_aborts(skip: u64, count: u64) {
            SKIP.store(skip, Ordering::Relaxed);
            INJECT.store(count, Ordering::Relaxed);
        }

        /// Injections armed but not yet consumed.
        pub fn pending_aborts() -> u64 {
            INJECT.load(Ordering::Relaxed)
        }

        fn take(counter: &AtomicU64) -> bool {
            counter
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
                .is_ok()
        }

        pub(in crate::arch::string_io) fn take_injection() -> bool {
            INJECT.load(Ordering::Relaxed) != 0 && !take(&SKIP) && take(&INJECT)
        }

        /// Run the real `rep insw` for `words` words from `port` into
        /// [`UNMAPPED`], with the forced fixup: it faults on the first store.
        ///
        /// # Safety
        /// Reads `port` like the caller's transfer would.
        pub unsafe fn raw_insw_unmapped(port: u16, words: u64) -> u64 {
            debug_assert!(!crate::arch::pagewalk::writable(UNMAPPED));
            force(true);
            // SAFETY: the destination is unmapped, so the first store faults
            // and the forced fixup returns before anything is written.
            let result =
                unsafe { super::super::lazyos_insw(u32::from(port), UNMAPPED as *mut u8, words) };
            force(false);
            result
        }

        /// An injected abort: see the module docs.
        ///
        /// # Safety
        /// Reads `port` like the caller's transfer would.
        pub(in crate::arch::string_io) unsafe fn inject(
            port: u16,
            words: u64,
        ) -> Result<(), super::super::Aborted> {
            // SAFETY: forwarded from the caller.
            match unsafe { raw_insw_unmapped(port, words) } {
                0 => Ok(()),
                _ => Err(super::super::Aborted),
            }
        }
    }
}
