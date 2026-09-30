//! Fault-storm detection (issue #373).
//!
//! The page-fault handler resumes the faulting instruction whenever a path
//! (copy-on-write, demand-zero, a `SIGSEGV` handler) reports the fault
//! handled. If that path is wrong, the instruction faults again at once and
//! the task livelocks: it stays Runnable, burns its whole share at one ring-3
//! `rip`, and the serial log says nothing. Counting identical consecutive
//! faults turns that silence into one decisive report line.
//!
//! Faults are handled with interrupts off on a single CPU, so plain atomics
//! with relaxed ordering are enough; they only need to be lock-free.

use core::sync::atomic::{AtomicU64, Ordering};

use x86_64::PhysAddr;

/// Identical faults in a row before reporting. A real access resolves in
/// one or two faults (demand-zero, then maybe COW), so this is far above any
/// legitimate sequence and far below a visible hang.
const THRESHOLD: u64 = 10_000;

/// Which handler path claimed the fault.
#[derive(Clone, Copy)]
pub enum Path {
    Cow,
    Demand,
    Signal,
}

impl Path {
    fn label(self) -> &'static str {
        match self {
            Path::Cow => "cow",
            Path::Demand => "demand",
            Path::Signal => "signal",
        }
    }
}

static LAST_CR3: AtomicU64 = AtomicU64::new(0);
static LAST_RIP: AtomicU64 = AtomicU64::new(0);
static LAST_ADDR: AtomicU64 = AtomicU64::new(0);
static REPEATS: AtomicU64 = AtomicU64::new(0);

/// Record a fault that `path` reported handled. Returns true exactly once
/// per storm, on the fault that crosses [`THRESHOLD`], after printing the
/// `FAULT:STORM` report.
pub fn note(table: PhysAddr, rip: u64, addr: u64, error: u64, path: Path) -> bool {
    let cr3 = table.as_u64();
    let same = LAST_CR3.load(Ordering::Relaxed) == cr3
        && LAST_RIP.load(Ordering::Relaxed) == rip
        && LAST_ADDR.load(Ordering::Relaxed) == addr;
    if !same {
        LAST_CR3.store(cr3, Ordering::Relaxed);
        LAST_RIP.store(rip, Ordering::Relaxed);
        LAST_ADDR.store(addr, Ordering::Relaxed);
        REPEATS.store(1, Ordering::Relaxed);
        return false;
    }
    let repeats = REPEATS.fetch_add(1, Ordering::Relaxed) + 1;
    if repeats != THRESHOLD {
        return false;
    }
    let chain = crate::mem::pte_chain(table, addr);
    let vma = crate::mem::vma::find(table, addr);
    crate::serial_println!(
        "FAULT:STORM task={} cr3={cr3:#x} rip={rip:#x} addr={addr:#x} error={error:#x} \
         path={} repeats={repeats} pml4e={:#x} pdpte={:#x} pde={:#x} pte={:#x} vma={vma:?}",
        crate::task::current(),
        path.label(),
        chain[0],
        chain[1],
        chain[2],
        chain[3],
    );
    true
}

/// Reset the detector (tests).
#[cfg(lazyos_tests)]
pub fn reset() {
    LAST_CR3.store(0, Ordering::Relaxed);
    LAST_RIP.store(0, Ordering::Relaxed);
    LAST_ADDR.store(0, Ordering::Relaxed);
    REPEATS.store(0, Ordering::Relaxed);
}
