//! In-kernel test harness and suite (issue #62).
//!
//! Compiled only when the image is built with `LAZYOS_TESTS=1`; `kernel_main`
//! then calls [`run`] instead of the normal boot. Every test prints exactly one
//! machine-parseable line to serial:
//!
//! ```text
//! TEST:<name>:PASS
//! TEST:<name>:FAIL:<detail>
//! TEST:SUMMARY:PASS=<n> FAIL=<n>
//! ```
//!
//! `tools/test/run.py` parses those lines and turns them into
//! `docs/test/report.md` + `docs/test/report.json`.
//!
//! The suite is split one file per subsystem under this directory (issue
//! #194), grouped here into [`SUITE`] in the order they must run: task tests
//! need the address-space helpers exercised first, and a couple of
//! `hardening_suite` regressions recurse until the kernel stack overflows, so
//! they stay last within their own suite's table. Add a new subsystem's tests
//! as its own `mod`, with a `CASES` table colocated with its test bodies, and
//! list it in [`SUITE`] below.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use x86_64::structures::idt::PageFaultErrorCode;
use x86_64::PhysAddr;

use crate::mem::vma::{Kind, Prot};
use crate::{mem, process, task};

/// A test body: `Err(detail)` fails the test.
type Test = fn() -> Result<(), String>;

// ---------------------------------------------------------------------------
// Shared helpers
//
// Every suite file below reaches these through `use super::*;`: privacy in
// Rust flows down the module tree, so items defined directly here (rather
// than re-exported from a child module) are visible to every suite module
// and their own sub-modules without any extra `pub` plumbing.
// ---------------------------------------------------------------------------

/// Raw page-table entry bits (the public CPU-visible layout; COW uses bit 9).
const PTE_PRESENT: u64 = 1 << 0;
const PTE_WRITABLE: u64 = 1 << 1;
const PTE_ADDR: u64 = 0x000F_FFFF_FFFF_F000;
const COW_BIT: u64 = 1 << 9;

/// First virtual address the suite uses for scratch user mappings.
const TEST_VA: u64 = 0x0040_0000;

macro_rules! check {
    ($cond:expr, $($arg:tt)*) => {
        if !($cond) {
            return Err(format!($($arg)*));
        }
    };
}

/// Walk `table` to the 4 KiB PTE for `va`, if mapped. Scratch mappings are 4 KiB;
/// huge-page entries are not expected.
fn raw_entry(table: PhysAddr, va: u64) -> Option<u64> {
    let mut phys = table.as_u64();
    let mut entry = 0u64;
    for shift in [39u64, 30, 21, 12] {
        let ptr = mem::phys_to_virt(PhysAddr::new(phys)).as_ptr::<u64>();
        // Safety: `phys` is a live page table reachable through the physical map.
        entry = unsafe { ptr.add(((va >> shift) & 0x1ff) as usize).read_volatile() };
        if entry & PTE_PRESENT == 0 {
            return None;
        }
        phys = entry & PTE_ADDR;
    }
    Some(entry)
}

/// Physical frame backing `va` in `table`.
fn frame_of(table: PhysAddr, va: u64) -> Result<u64, String> {
    let entry = raw_entry(table, va).ok_or_else(|| format!("no mapping for {va:#x}"))?;
    Ok(entry & PTE_ADDR)
}

/// Deterministic per-page fill pattern (never all-zero, so a stale zeroed frame
/// cannot pass by accident).
fn fill_frame(phys: u64, seed: u8) {
    let ptr = mem::phys_to_virt(PhysAddr::new(phys)).as_mut_ptr::<u8>();
    for i in 0..4096usize {
        // Safety: the frame is mapped writable through the physical memory map.
        unsafe { ptr.add(i).write_volatile(pattern_byte(seed, i)) };
    }
}

/// Whether `phys` still holds the pattern written by [`fill_frame`].
fn frame_matches(phys: u64, seed: u8) -> bool {
    let ptr = mem::phys_to_virt(PhysAddr::new(phys)).as_ptr::<u8>();
    for i in 0..4096usize {
        // Safety: the frame is mapped readable through the physical memory map.
        let got = unsafe { ptr.add(i).read_volatile() };
        if got != pattern_byte(seed, i) {
            return false;
        }
    }
    true
}

fn pattern_byte(seed: u8, index: usize) -> u8 {
    seed ^ (index as u8).wrapping_mul(31)
}

fn page_seed(iteration: u32, page: usize) -> u8 {
    (iteration as u8).wrapping_mul(31) ^ (page as u8).wrapping_mul(17)
}

fn to_string(error: &'static str) -> String {
    error.into()
}

mod acl_suite;
mod arch_suite;
mod block_suite;
mod boot_io_suite;
mod boot_trace_suite;
mod confd_suite;
mod credentials_suite;
mod crypto_suite;
mod dev_suite;
mod display_suite;
mod ext2_suite;
mod fault_suite;
mod fs_suite;
mod fsops_suite;
mod hardening_suite;
mod heap_suite;
mod input_bus_suite;
mod ipc_channel_suite;
mod ipc_shared_suite;
mod ipc_suite;
mod keyboard_suite;
mod label_suite;
mod linux_suite;
mod loader_suite;
mod mem_suite;
mod messenger_suite;
mod mount_suite;
mod native_exec_suite;
mod overlay_suite;
mod partition_suite;
mod pipe_suite;
mod preempt_lock_suite;
mod quota_suite;
mod ramdisk_suite;
mod registry_suite;
mod sched_suite;
mod service_suite;
mod signal_suite;
mod slab_suite;
mod spawn_argv_suite;
mod spurious_fault_suite;
mod stats_suite;
mod string_io_suite;
mod sysinfo_suite;
mod task_suite;
mod timed_suite;
mod topics_gate_suite;
mod topics_suite;
mod virtio_suite;
mod wallclock_suite;

/// Every suite, run in the order listed. See the module doc for why the
/// order of suites (and of a few tests within `hardening_suite`) is load-bearing.
const SUITE: &[&[(&str, Test)]] = &[
    mem_suite::CASES,
    heap_suite::CASES,
    arch_suite::CASES,
    preempt_lock_suite::CASES,
    slab_suite::CASES,
    quota_suite::CASES,
    task_suite::CASES,
    pipe_suite::CASES,
    linux_suite::CASES,
    loader_suite::CASES,
    sched_suite::CASES,
    signal_suite::CASES,
    fault_suite::CASES,
    ipc_suite::CASES,
    ipc_channel_suite::CASES,
    acl_suite::CASES,
    credentials_suite::CASES,
    label_suite::CASES,
    ipc_shared_suite::CASES,
    crypto_suite::CASES,
    messenger_suite::CASES,
    stats_suite::CASES,
    keyboard_suite::CASES,
    input_bus_suite::CASES,
    registry_suite::CASES,
    confd_suite::CASES,
    block_suite::CASES,
    virtio_suite::CASES,
    partition_suite::CASES,
    mount_suite::CASES,
    boot_trace_suite::CASES,
    boot_io_suite::CASES,
    string_io_suite::CASES,
    spurious_fault_suite::CASES,
    ramdisk_suite::CASES,
    dev_suite::CORE,
    dev_suite::CLASS_MAP,
    dev_suite::IRQ,
    dev_suite::IRQ_SHARED,
    dev_suite::IRQ_EDGE,
    dev_suite::IRQ_REAL,
    dev_suite::SYSCALL,
    dev_suite::SYSCALL_GUARD,
    dev_suite::SYSCALL_OPS,
    dev_suite::SYSCALL_POLICY,
    dev_suite::SYSCALL_CFG,
    dev_suite::TEARDOWN,
    dev_suite::DMA,
    dev_suite::DMA_LIFE,
    dev_suite::DMA_STRESS,
    dev_suite::STRESS,
    fs_suite::CASES,
    fsops_suite::CASES,
    overlay_suite::CASES,
    ext2_suite::CASES,
    ext2_suite::data_fds::CASES,
    topics_suite::CASES,
    topics_gate_suite::CASES,
    service_suite::CASES,
    spawn_argv_suite::CASES,
    native_exec_suite::CASES,
    display_suite::CASES,
    sysinfo_suite::CASES,
    wallclock_suite::CASES,
    timed_suite::CASES,
    hardening_suite::CASES,
];

/// Run the suite, print the results, and halt.
pub fn run() -> ! {
    serial_println!("LazyOS: kernel test mode (LAZYOS_TESTS=1)");
    // Tests build address spaces and task frames, so load the GDT/IDT/PIT as a
    // normal boot does. Interrupts stay disabled and the scheduler never starts.
    crate::arch::init();

    let mut pass = 0usize;
    let mut fail = 0usize;
    // `LAZYOS_TEST_FILTER=inet cargo build` (or `tools/test/run.py` with the
    // variable set) runs only the tests whose name contains the text, for a
    // quick turn while working on one subsystem. Unset, everything runs.
    let filter = option_env!("LAZYOS_TEST_FILTER").unwrap_or("");
    for suite in SUITE {
        for (name, test) in *suite {
            if !name.contains(filter) {
                continue;
            }
            match test() {
                Ok(()) => {
                    pass += 1;
                    serial_println!("TEST:{name}:PASS");
                }
                Err(detail) => {
                    fail += 1;
                    serial_println!("TEST:{name}:FAIL:{}", detail.replace('\n', " "));
                }
            }
        }
    }
    serial_println!("TEST:SUMMARY:PASS={pass} FAIL={fail}");
    debug_exit(fail == 0);
    crate::halt();
}

/// Ask QEMU to exit through `isa-debug-exit` (issue #9): `cargo run --
/// --headless` then ends with the suite's verdict as its exit status (0x10 is
/// success, 0x11 failure; QEMU reports `(value << 1) | 1`). Without that
/// device attached (the tools/ runners) the write is ignored and the caller
/// halts as before.
fn debug_exit(success: bool) {
    const ISA_DEBUG_EXIT_PORT: u16 = 0xf4;
    let code: u32 = if success { 0x10 } else { 0x11 };
    // SAFETY: port 0xf4 is the isa-debug-exit device's register when present
    // and unclaimed otherwise; writing to it has no effect beyond ending the VM.
    unsafe { crate::arch::io::outl(ISA_DEBUG_EXIT_PORT, code) };
}
