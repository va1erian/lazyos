//! Spurious ring-3 page faults from the hypervisor's instruction emulator.
//!
//! Under WHPX, an instruction the hypervisor emulates (`rep movsb` touching
//! device memory, string I/O) can come back with a fabricated `#PF` for an
//! address the guest's page tables map correctly; the ring-0 flavour is
//! handled in [`super::string_io`]. For a user task the fabricated fault has
//! no P bit (RSVD alone is impossible on hardware), so the demand-zero path
//! read it as "page not present", failed to map over the existing page and
//! killed the process with SIGSEGV on a perfectly valid `movsb`.
//!
//! The page tables are the authority: if they already permit the faulting
//! access, the fault cannot be real and the instruction is simply retried.
//! Faults on pages the tables do not permit (demand-zero, copy-on-write,
//! genuinely bad accesses) are untouched.

use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::structures::idt::PageFaultErrorCode;

/// Spurious faults retried since boot; the first few are also logged.
static RETRIED: AtomicU64 = AtomicU64::new(0);
const LOGGED: u64 = 4;

/// Whether a fault with `error` at `addr`, raised from ring 3, is one the
/// current page tables already permit.
pub fn is_spurious(error: PageFaultErrorCode, addr: u64) -> bool {
    super::pagewalk::user_access_allowed(
        addr,
        error.contains(PageFaultErrorCode::CAUSED_BY_WRITE),
        error.contains(PageFaultErrorCode::INSTRUCTION_FETCH),
    )
}

/// Record a retried spurious fault (log the first few).
pub fn note(error: PageFaultErrorCode, addr: u64, rip: u64) {
    let count = RETRIED.fetch_add(1, Ordering::Relaxed) + 1;
    if count <= LOGGED {
        crate::serial_println!(
            "user: spurious #PF at {addr:#x} ({error:?}), rip {rip:#x}:              page is mapped, hypervisor emulation fault, retrying ({count} so far)"
        );
    }
}

/// Spurious user faults retried since boot.
#[cfg(lazyos_tests)]
pub fn retried() -> u64 {
    RETRIED.load(Ordering::Relaxed)
}
