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
//! The suite covers what is deterministic in kernel context: the frame
//! allocator's public API, address-space construction, copy-on-write fork churn
//! (including a deliberate soak loop), the kernel heap, and task/futex/fd
//! bookkeeping. Allocator-internals tests (e.g. the counters reworked by #54)
//! belong in [`mem_suite`] so they can be added without touching the harness.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use x86_64::PhysAddr;

use crate::{mem, process, task};

/// A test body: `Err(detail)` fails the test.
type Test = fn() -> Result<(), String>;

/// The suite, run in order. Task tests run last: they register the kernel task
/// and need the address-space helpers to be exercised first.
const SUITE: &[(&str, Test)] = &[
    (
        "mem_frames_distinct_aligned",
        mem_suite::frames_distinct_aligned,
    ),
    ("mem_zeroed_frame_clear", mem_suite::zeroed_frame_clear),
    (
        "mem_user_table_shares_kernel_half",
        mem_suite::user_table_shares_kernel_half,
    ),
    (
        "mem_cow_clone_copies_on_write",
        mem_suite::cow_clone_copies_on_write,
    ),
    ("mem_soak_cow_fork_churn", mem_suite::soak_cow_fork_churn),
    ("heap_vec_integrity", heap_suite::vec_integrity),
    ("task_kernel_registered", task_suite::kernel_registered),
    (
        "task_block_wake_roundtrip",
        task_suite::block_wake_roundtrip,
    ),
    ("task_fork_reap_churn", task_suite::fork_reap_churn),
    ("task_futex_wait_mismatch", task_suite::futex_wait_mismatch),
    ("task_fd_table", task_suite::fd_table),
];

/// Run the suite, print the results, and halt.
pub fn run() -> ! {
    serial_println!("LazyOS: kernel test mode (LAZYOS_TESTS=1)");
    // Tests build address spaces and task frames, so load the GDT/IDT/PIT as a
    // normal boot does. Interrupts stay disabled and the scheduler never starts.
    crate::arch::init();

    let mut pass = 0usize;
    let mut fail = 0usize;
    for (name, test) in SUITE {
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
    serial_println!("TEST:SUMMARY:PASS={pass} FAIL={fail}");
    crate::halt();
}

// ---------------------------------------------------------------------------
// Shared helpers
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

// ---------------------------------------------------------------------------
// Frame allocator and address spaces
// ---------------------------------------------------------------------------

mod mem_suite {
    use super::*;

    /// `alloc_frame` hands out distinct, 4 KiB-aligned frames outside low memory.
    ///
    /// #54 replaces the bump allocator; keep this on the public API only, and
    /// add counter assertions (e.g. `allocated()`) in a new test here.
    pub fn frames_distinct_aligned() -> Result<(), String> {
        let mut seen: Vec<u64> = Vec::new();
        for index in 0..64 {
            let phys = mem::alloc_frame()
                .ok_or_else(|| format!("frame {index}: alloc_frame returned None"))?;
            let address = phys.as_u64();
            check!(
                address & 0xfff == 0,
                "frame {index} {address:#x} is not 4 KiB aligned"
            );
            check!(
                address >= 0x10_0000,
                "frame {index} {address:#x} is in low memory"
            );
            check!(
                !seen.contains(&address),
                "frame {index} {address:#x} was handed out twice"
            );
            seen.push(address);
        }
        Ok(())
    }

    /// `alloc_zeroed_frame` maps a clear frame readable/writable via `phys_to_virt`.
    pub fn zeroed_frame_clear() -> Result<(), String> {
        let phys = mem::alloc_zeroed_frame().ok_or("alloc_zeroed_frame returned None")?;
        let ptr = mem::phys_to_virt(phys).as_mut_ptr::<u8>();
        for i in 0..4096usize {
            // Safety: the frame is freshly allocated and mapped writable.
            let byte = unsafe { ptr.add(i).read_volatile() };
            check!(byte == 0, "zeroed frame byte {i} is {byte:#x}");
        }
        for i in (0..4096).step_by(64) {
            // Safety: as above.
            unsafe { ptr.add(i).write_volatile((i & 0xff) as u8) };
        }
        for i in (0..4096).step_by(64) {
            // Safety: as above.
            let got = unsafe { ptr.add(i).read_volatile() };
            check!(
                got == (i & 0xff) as u8,
                "frame round-trip at {i} got {got:#x}"
            );
        }
        Ok(())
    }

    /// A fresh user table shares every kernel-half PML4 entry and has an empty
    /// user half.
    pub fn user_table_shares_kernel_half() -> Result<(), String> {
        let table = mem::new_user_table().ok_or("new_user_table returned None")?;
        let kernel = mem::kernel_table();
        // Safety: both are live PML4 frames mapped through the physical map.
        let kernel_entries =
            unsafe { core::slice::from_raw_parts(mem::phys_to_virt(kernel).as_ptr::<u64>(), 512) };
        // Safety: as above.
        let user_entries =
            unsafe { core::slice::from_raw_parts(mem::phys_to_virt(table).as_ptr::<u64>(), 512) };
        check!(
            user_entries[0] & PTE_PRESENT == 0,
            "new user table entry 0 is present: {:#x}",
            user_entries[0]
        );
        for index in 1..512 {
            check!(
                user_entries[index] == kernel_entries[index],
                "PML4 entry {index} differs: kernel {:#x}, new table {:#x}",
                kernel_entries[index],
                user_entries[index]
            );
        }
        Ok(())
    }

    /// COW fork: clone marks both sides read-only, and the first writer on each
    /// side gets a private copy with the original contents.
    pub fn cow_clone_copies_on_write() -> Result<(), String> {
        let parent = mem::new_user_table().ok_or("new_user_table failed")?;
        let pages = process::map_range(parent, TEST_VA, TEST_VA + 2 * 4096).map_err(to_string)?;
        check!(
            pages.len() == 2,
            "map_range mapped {} pages, expected 2",
            pages.len()
        );
        for (index, (_, phys)) in pages.iter().enumerate() {
            fill_frame(*phys, page_seed(0, index));
        }

        let child = mem::clone_user_table(parent).ok_or("clone_user_table failed")?;
        for (index, (va, parent_phys)) in pages.iter().enumerate() {
            let parent_entry =
                raw_entry(parent, *va).ok_or_else(|| format!("parent lost page {index}"))?;
            let child_entry =
                raw_entry(child, *va).ok_or_else(|| format!("child missing page {index}"))?;
            check!(
                parent_entry & PTE_WRITABLE == 0 && parent_entry & COW_BIT != 0,
                "parent page {index} is not COW read-only after clone: {parent_entry:#x}"
            );
            check!(
                child_entry & PTE_WRITABLE == 0 && child_entry & COW_BIT != 0,
                "child page {index} is not COW read-only after clone: {child_entry:#x}"
            );
            check!(
                child_entry & PTE_ADDR == *parent_phys,
                "child page {index} does not share the parent frame"
            );
        }

        for (index, (va, shared_phys)) in pages.iter().enumerate() {
            let seed = page_seed(0, index);
            check!(
                mem::cow_fault(child, *va),
                "cow_fault failed on the child, page {index}"
            );
            let child_phys = frame_of(child, *va)?;
            check!(
                child_phys != *shared_phys,
                "child page {index} still shares the parent frame"
            );
            check!(
                frame_matches(child_phys, seed),
                "child copy of page {index} is corrupted"
            );
            check!(
                frame_matches(*shared_phys, seed),
                "parent frame for page {index} changed"
            );

            check!(
                mem::cow_fault(parent, *va),
                "cow_fault failed on the parent, page {index}"
            );
            let parent_phys = frame_of(parent, *va)?;
            check!(
                parent_phys != child_phys && parent_phys != *shared_phys,
                "parent page {index} was not copied"
            );
            check!(
                frame_matches(parent_phys, seed),
                "parent copy of page {index} is corrupted"
            );
        }

        // A write through the child's private copy must not reach the parent.
        let (va, _) = pages[0];
        let child_phys = frame_of(child, va)?;
        let scratch = mem::phys_to_virt(PhysAddr::new(child_phys)).as_mut_ptr::<u8>();
        // Safety: the frame is private to the child at this point.
        unsafe { scratch.write_volatile(0xEE) };
        check!(
            frame_matches(frame_of(parent, va)?, page_seed(0, 0)),
            "writing the child copy modified the parent"
        );
        Ok(())
    }

    /// Soak: 500 fork/COW/write cycles, both directions, with progress and a
    /// cycle-budget verdict. The runner's wall-clock timeout is the second bound.
    pub fn soak_cow_fork_churn() -> Result<(), String> {
        const ITERATIONS: u32 = 500;
        const PAGES: u64 = 3;
        /// A deliberately generous ceiling (roughly a minute of wall clock);
        /// the loop is expected to take well under a second even under TCG.
        const MAX_CYCLES: u64 = 200_000_000_000;

        let start = unsafe { core::arch::x86_64::_rdtsc() };
        for iteration in 0..ITERATIONS {
            let parent = mem::new_user_table()
                .ok_or_else(|| format!("iteration {iteration}: new_user_table failed"))?;
            let pages = process::map_range(parent, TEST_VA, TEST_VA + PAGES * 4096)
                .map_err(|error| format!("iteration {iteration}: {error}"))?;
            for (index, (_, phys)) in pages.iter().enumerate() {
                fill_frame(*phys, page_seed(iteration, index));
            }

            let child = mem::clone_user_table(parent)
                .ok_or_else(|| format!("iteration {iteration}: clone_user_table failed"))?;
            for (index, (va, shared_phys)) in pages.iter().enumerate() {
                let seed = page_seed(iteration, index);
                check!(
                    mem::cow_fault(child, *va),
                    "iteration {iteration}: child cow_fault failed, page {index}"
                );
                let child_phys = frame_of(child, *va)?;
                check!(
                    child_phys != *shared_phys,
                    "iteration {iteration}: child page {index} not copied"
                );
                check!(
                    frame_matches(child_phys, seed),
                    "iteration {iteration}: child copy of page {index} corrupted"
                );

                check!(
                    mem::cow_fault(parent, *va),
                    "iteration {iteration}: parent cow_fault failed, page {index}"
                );
                let parent_phys = frame_of(parent, *va)?;
                check!(
                    parent_phys != child_phys && parent_phys != *shared_phys,
                    "iteration {iteration}: parent page {index} not copied"
                );
                check!(
                    frame_matches(parent_phys, seed),
                    "iteration {iteration}: parent copy of page {index} corrupted"
                );
            }
            if iteration % 100 == 0 {
                serial_println!(
                    "TEST:mem_soak_cow_fork_churn:PROGRESS:iteration {iteration}/{ITERATIONS}"
                );
            }
        }
        let cycles = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start);
        serial_println!(
            "TEST:mem_soak_cow_fork_churn:INFO:iterations={ITERATIONS} pages={PAGES} cycles={cycles}"
        );
        check!(
            cycles < MAX_CYCLES,
            "soak used {cycles} cycles, over the {MAX_CYCLES} budget"
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Kernel heap
// ---------------------------------------------------------------------------

mod heap_suite {
    use super::*;

    /// Allocating, filling, dropping and reusing heap blocks keeps data intact.
    pub fn vec_integrity() -> Result<(), String> {
        let mut buffers: Vec<Vec<u8>> = Vec::with_capacity(64);
        for round in 0..64u32 {
            let mut buffer = Vec::with_capacity(4096);
            for index in 0..4096u32 {
                buffer.push((round as u8) ^ (index as u8).wrapping_mul(7));
            }
            buffers.push(buffer);
        }
        for (round, buffer) in buffers.iter().enumerate() {
            check!(
                buffer.len() == 4096,
                "round {round} length is {}",
                buffer.len()
            );
            for (index, &byte) in buffer.iter().enumerate() {
                check!(
                    byte == (round as u8) ^ (index as u8).wrapping_mul(7),
                    "round {round} byte {index} is {byte:#x} (heap corruption?)"
                );
            }
        }
        drop(buffers);

        // Freed blocks should be reusable and still hold what we write.
        let mut reused: Vec<u8> = Vec::with_capacity(4096);
        for index in 0..4096u32 {
            reused.push((index as u8) ^ 0x5a);
        }
        check!(
            reused.len() == 4096,
            "reallocated length is {}",
            reused.len()
        );
        check!(
            reused[2048] == (2048u32 as u8) ^ 0x5a,
            "reallocated buffer is corrupted"
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Task bookkeeping
// ---------------------------------------------------------------------------

mod task_suite {
    use super::*;

    /// Registering the kernel task sets the current slot and snapshot fields.
    pub fn kernel_registered() -> Result<(), String> {
        task::register_kernel();
        check!(
            task::current() == task::KERNEL_TASK,
            "current task is {}, expected the kernel slot",
            task::current()
        );
        let (name, _, done) =
            task::snapshot(task::KERNEL_TASK).ok_or("kernel task is not registered")?;
        check!(name == "kernel", "kernel task is named {name:?}");
        check!(!done, "kernel task starts done");
        check!(
            !task::has_children(),
            "kernel task unexpectedly has children"
        );
        check!(
            task::reap_child().is_none(),
            "kernel task reaped a nonexistent child"
        );
        Ok(())
    }

    /// The futex park/wake mechanism: `set_blocked` parks, `wake_task` resumes.
    pub fn block_wake_roundtrip() -> Result<(), String> {
        check!(!task::blocked(), "task starts blocked");
        task::set_blocked(true);
        check!(task::blocked(), "set_blocked(true) did not park the task");
        task::wake_task(task::current());
        check!(!task::blocked(), "wake_task did not resume the task");
        Ok(())
    }

    /// `spawn_fork` bookkeeping, exit, and reaping across many rounds.
    pub fn fork_reap_churn() -> Result<(), String> {
        task::harness::reset();
        for round in 0..8u32 {
            let slot = task::spawn_fork()
                .map_err(|error| format!("round {round}: spawn_fork: {error}"))?;
            check!(
                (1..task::MAX_TASKS).contains(&slot),
                "round {round}: fork slot {slot} is out of range"
            );
            check!(
                task::reap_child().is_none(),
                "round {round}: reaped a child that is still running"
            );
            task::harness::finish(slot, 0x40 + round as u64);
            let (reaped, status) = task::reap_child()
                .ok_or_else(|| format!("round {round}: child is not reapable"))?;
            check!(
                reaped == slot,
                "round {round}: reaped slot {reaped}, expected {slot}"
            );
            check!(
                status == 0x40 + round as u64,
                "round {round}: exit status {status:#x}"
            );
        }
        task::harness::reset();
        Ok(())
    }

    /// `futex(FUTEX_WAIT)` on a mismatched word returns EAGAIN without blocking;
    /// `FUTEX_WAKE` with no waiters returns 0.
    pub fn futex_wait_mismatch() -> Result<(), String> {
        let mut word: u32 = 7;
        let addr = core::ptr::addr_of_mut!(word) as u64;
        let eagain = (-11i64) as u64;
        let result = process::linux::dispatch_for_test(202, addr, 0, 99);
        check!(
            result == eagain,
            "futex WAIT on a mismatched word returned {result:#x}, expected EAGAIN"
        );
        let woken = process::linux::dispatch_for_test(202, addr, 1, 1);
        check!(woken == 0, "futex WAKE with no waiters returned {woken}");
        check!(word == 7, "futex syscall modified the word");
        Ok(())
    }

    /// Open, size, read, seek, duplicate and close a descriptor.
    pub fn fd_table() -> Result<(), String> {
        task::register_kernel();
        let fd = task::fd_open(task::Fd::File {
            data: b"hello".to_vec(),
            offset: 0,
        })
        .ok_or("fd_open failed")?;
        check!(fd >= 3, "fd_open returned reserved slot {fd}");
        check!(
            task::fd_kind(fd) == task::FdKind::File,
            "opened fd is not a file"
        );
        check!(
            task::fd_size(fd) == Some(5),
            "file size is {:?}, expected 5",
            task::fd_size(fd)
        );

        let mut buffer = [0u8; 8];
        let read = task::fd_read(fd, buffer.as_mut_ptr(), buffer.len()).ok_or("fd_read failed")?;
        check!(
            read == 5 && &buffer[..5] == b"hello",
            "fd_read got {read} bytes: {:?}",
            &buffer[..read.min(buffer.len())]
        );

        check!(task::fd_seek(fd, 0, 0) == Some(0), "fd_seek(SET 0) failed");
        let duplicate = task::fd_dup(fd).ok_or("fd_dup failed")?;
        check!(duplicate != fd, "fd_dup reused the same slot");
        check!(
            task::fd_size(duplicate) == Some(5),
            "duplicated fd lost its data"
        );
        check!(task::fd_close(duplicate), "fd_close(duplicate) failed");
        check!(
            task::fd_kind(duplicate) == task::FdKind::Closed,
            "closed fd is still open"
        );
        check!(task::fd_close(fd), "fd_close(fd) failed");
        check!(
            !task::fd_close(fd),
            "closing an already closed fd succeeded"
        );
        Ok(())
    }
}
