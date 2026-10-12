//! Executable memory under fork and under load (issue #773): a forked RX page
//! stays RX and is copied only by the first store after `mprotect(RW)`; and
//! 100k W^X flips of one page that a second thread executes in a loop.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use super::execmem::{call, emit, mmap_prot, mprotect, unmap, NX, PAGE, RW, RX};
use super::*;
use crate::task::wait::WaitQueue;
use crate::task::{PriorityClass, WaitKind};

fn exec_bits(table: PhysAddr, va: u64) -> Result<(bool, bool, bool), String> {
    let entry = raw_entry(table, va).ok_or("page vanished")?;
    Ok((
        entry & NX == 0,
        entry & PTE_WRITABLE != 0,
        entry & COW_BIT != 0,
    ))
}

fn frame_code(frame: u64) -> u32 {
    let ptr = mem::phys_to_virt(PhysAddr::new(frame)).as_ptr::<u8>();
    // SAFETY: the frame is mapped through the physical-memory window.
    unsafe {
        assert_eq!(ptr.read_volatile(), 0xB8);
        u32::from_le_bytes([
            ptr.add(1).read_volatile(),
            ptr.add(2).read_volatile(),
            ptr.add(3).read_volatile(),
            ptr.add(4).read_volatile(),
        ])
    }
}

fn set_frame_code(frame: u64, imm: u32) {
    let ptr = mem::phys_to_virt(PhysAddr::new(frame)).as_mut_ptr::<u8>();
    let mut code = [0xB8, 0, 0, 0, 0, 0xC3];
    code[1..5].copy_from_slice(&imm.to_le_bytes());
    for (i, byte) in code.iter().enumerate() {
        // SAFETY: the frame is mapped through the physical-memory window.
        unsafe { ptr.add(i).write_volatile(*byte) };
    }
}

fn set_prot(table: PhysAddr, va: u64, prot: Prot) -> Result<(), String> {
    check!(
        mem::protect_range(table, va, va + PAGE, prot),
        "protect_range failed"
    );
    mem::vma::protect(table, va, va + PAGE, prot);
    Ok(())
}

/// Fork shares a JIT page read-only and executable in both processes; a
/// store to it is never a COW request while the VMA says RX; a child that
/// flips it RW -> RX copies it only at its first store, and its patch never
/// reaches the parent.
pub fn execmem_fork_inherits_rx_and_cows_on_write() -> Result<(), String> {
    let free = mem::frame_stats().free;
    let rw = Prot::READ | Prot::WRITE;
    let rx = Prot::READ | Prot::EXEC;
    let parent = mem::new_user_table().ok_or("new_user_table failed")?;
    let base = TEST_VA;
    mem::vma::insert(parent, base, base + PAGE, rw, Kind::Anon);
    check!(
        mem::demand_fault(parent, base, PageFaultErrorCode::CAUSED_BY_WRITE),
        "demand fault failed"
    );
    let shared = frame_of(parent, base)?;
    set_frame_code(shared, 41);
    set_prot(parent, base, rx)?;
    check!(
        exec_bits(parent, base)? == (true, false, false),
        "parent page is not private RX"
    );

    let child = mem::clone_user_table(parent).ok_or("clone_user_table failed")?;
    for (name, table) in [("parent", parent), ("child", child)] {
        check!(
            frame_of(table, base)? == shared,
            "{name} lost the shared frame"
        );
        check!(
            exec_bits(table, base)? == (true, false, true),
            "{name}: a forked RX page is not shared RX+COW: {:?}",
            exec_bits(table, base)
        );
        check!(
            !mem::cow_fault(table, base),
            "{name}: a store to a shared RX page was granted a copy"
        );
    }

    // The child flips RX -> RW: shared and read-only until it writes.
    set_prot(child, base, rw)?;
    check!(
        frame_of(child, base)? == shared && exec_bits(child, base)? == (false, false, true),
        "mprotect(RW) copied or exposed a shared page"
    );
    check!(
        mem::cow_fault(child, base),
        "the child's first store found no COW"
    );
    let private = frame_of(child, base)?;
    check!(
        private != shared && exec_bits(child, base)? == (false, true, false),
        "the child did not get a private writable page"
    );
    check!(frame_code(private) == 41, "the copy lost the code");

    // The child patches its copy and flips back to RX; the parent is unmoved.
    set_frame_code(private, 42);
    set_prot(child, base, rx)?;
    check!(
        exec_bits(child, base)? == (true, false, false) && frame_code(private) == 42,
        "child RX again"
    );
    check!(
        frame_of(parent, base)? == shared
            && frame_code(shared) == 41
            && exec_bits(parent, base)? == (true, false, true),
        "the child's flip changed the parent"
    );

    // The parent is now the frame's only owner: RW is granted in place.
    check!(mem::frame_refcount(PhysAddr::new(shared)) == 1, "refcount");
    set_prot(parent, base, rw)?;
    check!(
        frame_of(parent, base)? == shared && exec_bits(parent, base)? == (false, true, false),
        "a sole owner was copied or left read-only"
    );

    for table in [parent, child] {
        mem::unmap_range(table, base, base + PAGE);
        mem::free_user_table(table);
    }
    check!(
        mem::frame_stats().free == free,
        "the fork test leaked frames"
    );
    Ok(())
}

/// Flips per soak. The issue asks for 100k.
const FLIPS: u64 = 100_000;
/// Bytes of `nop` between `mov eax, imm` and `ret`, so a preemption is likely
/// to land inside the page.
const SLED: usize = 250;

static CODE_PAGE: AtomicU64 = AtomicU64::new(0);
/// The runner may enter the page only while this is clear; set by the flipper
/// for the whole RW window.
static WANT_RW: AtomicBool = AtomicBool::new(false);
/// The runner is between its entry check and the return from the page.
static INSIDE: AtomicBool = AtomicBool::new(false);
static READY: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
/// The value the page returns right now.
static GEN: AtomicU32 = AtomicU32::new(0);
/// Calls made, calls that saw a value other than `GEN`, generations seen.
static CALLS: AtomicU64 = AtomicU64::new(0);
static WRONG: AtomicU64 = AtomicU64::new(0);
static CHANGES: AtomicU64 = AtomicU64::new(0);
static PARK: WaitQueue = WaitQueue::new(WaitKind::Sleep);

/// `mov eax, imm; nop...; ret` over the first `SLED + 6` bytes.
fn emit_sled(addr: u64, imm: u32) {
    emit(addr, imm);
    for i in 0..SLED {
        // SAFETY: the caller maps `addr` writable in the kernel's user half.
        unsafe { ((addr + 5 + i as u64) as *mut u8).write_volatile(0x90) };
    }
    // SAFETY: as above.
    unsafe { ((addr + 5 + SLED as u64) as *mut u8).write_volatile(0xC3) };
}

/// Change only the immediate, as a JIT patches a constant.
fn patch_imm(addr: u64, imm: u32) {
    for (i, byte) in imm.to_le_bytes().iter().enumerate() {
        // SAFETY: the caller maps `addr` writable in the kernel's user half.
        unsafe { ((addr + 1 + i as u64) as *mut u8).write_volatile(*byte) };
    }
}

/// The executing thread: while the page is RX (and the flipper is not in its
/// RW window) call it and check it returns the current generation.
///
/// It runs with interrupts off and hands the CPU over with `yield_now`, so the
/// two threads interleave at known points (a thread that enabled interrupts
/// and was finished mid-slice left later timing tests flaky). Preemption
/// landing inside the page is covered in ring 3 by the `jitmem` ABI fixture.
extern "C" fn runner() -> ! {
    let me = task::current();
    let mut last = u32::MAX;
    loop {
        if STOP.load(Ordering::SeqCst) {
            PARK.wait(me, None);
            continue;
        }
        if WANT_RW.load(Ordering::SeqCst) || !READY.load(Ordering::SeqCst) {
            task::switch::yield_now();
            continue;
        }
        // Dekker: announce, then re-check, so the flipper either sees us
        // inside (and waits) or we see its request (and stay out).
        INSIDE.store(true, Ordering::SeqCst);
        if WANT_RW.load(Ordering::SeqCst) {
            INSIDE.store(false, Ordering::SeqCst);
            continue;
        }
        let expect = GEN.load(Ordering::SeqCst);
        let got = call(CODE_PAGE.load(Ordering::SeqCst));
        INSIDE.store(false, Ordering::SeqCst);
        if got != expect {
            WRONG.fetch_add(1, Ordering::SeqCst);
        }
        if expect != last {
            last = expect;
            CHANGES.fetch_add(1, Ordering::SeqCst);
        }
        if CALLS.fetch_add(1, Ordering::SeqCst) % 16 == 15 {
            task::switch::yield_now();
        }
    }
}

/// The flipper: RW -> patch -> RX, `FLIPS` times, never while the runner is
/// inside the page.
fn flip_loop(page: u64) -> Result<(), String> {
    for flip in 1..=FLIPS {
        let imm = flip as u32;
        WANT_RW.store(true, Ordering::SeqCst);
        while INSIDE.load(Ordering::SeqCst) {
            task::switch::yield_now();
        }
        check!(mprotect(page, PAGE, RW) == 0, "flip {flip}: mprotect(RW)");
        patch_imm(page, imm);
        check!(mprotect(page, PAGE, RX) == 0, "flip {flip}: mprotect(RX)");
        GEN.store(imm, Ordering::SeqCst);
        READY.store(true, Ordering::SeqCst);
        WANT_RW.store(false, Ordering::SeqCst);
        if flip % 4 == 0 {
            task::switch::yield_now();
        }
    }
    Ok(())
}

/// 100k RW <-> RX flips on a page a second thread executes in a loop: the
/// thread only ever sees the code of the current generation, runs plenty of
/// it, and after the final `munmap` no mapping, VMA or frame is left.
pub fn execmem_soak_wx_flips_with_second_thread() -> Result<(), String> {
    fresh()?;
    // Warm-up: build the page tables the page lives under, so the frame
    // count compared below is not moved by tables that stay behind by design.
    let warm = mmap_prot(PAGE, RW)?;
    emit_sled(warm, 1);
    check!(unmap(warm, PAGE) == 0, "warm-up munmap");

    for counter in [&CALLS, &WRONG, &CHANGES, &CODE_PAGE] {
        counter.store(0, Ordering::SeqCst);
    }
    for flag in [&WANT_RW, &INSIDE, &READY, &STOP] {
        flag.store(false, Ordering::SeqCst);
    }
    let slot =
        task::kthread::spawn_kernel_thread("execmem-runner", runner, PriorityClass::Interactive)
            .map_err(|error| format!("spawn: {error}"))?;
    let before = (mem::frame_stats().free, mem::vma_stats(mem::kernel_table()));

    let outcome = (|| {
        let page = mmap_prot(PAGE, RW)?;
        CODE_PAGE.store(page, Ordering::SeqCst);
        emit_sled(page, 0);
        check!(mprotect(page, PAGE, RX) == 0, "first mprotect(RX)");
        GEN.store(0, Ordering::SeqCst);
        READY.store(true, Ordering::SeqCst);
        flip_loop(page)?;
        // Leave it RW (as a JIT does between patches), then unmap.
        WANT_RW.store(true, Ordering::SeqCst);
        while INSIDE.load(Ordering::SeqCst) {
            task::switch::yield_now();
        }
        check!(mprotect(page, PAGE, RW) == 0, "final mprotect(RW)");
        check!(unmap(page, PAGE) == 0, "final munmap");
        check!(
            raw_entry(mem::kernel_table(), page).is_none()
                && mem::vma::find(mem::kernel_table(), page).is_none(),
            "a stale mapping or VMA survived the munmap"
        );
        let after = (mem::frame_stats().free, mem::vma_stats(mem::kernel_table()));
        check!(after == before, "leak: {before:?} -> {after:?}");
        Ok(())
    })();

    // End the runner: park it, finish and reap it, as the other thread tests do.
    STOP.store(true, Ordering::SeqCst);
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::finish(slot, 0);
    PARK.notify_all();
    let reaped = task::reap_child();
    task::harness::reset();
    outcome?;

    check!(reaped.is_some(), "the runner was not reapable");
    let (calls, wrong, changes) = (
        CALLS.load(Ordering::SeqCst),
        WRONG.load(Ordering::SeqCst),
        CHANGES.load(Ordering::SeqCst),
    );
    check!(
        wrong == 0,
        "{wrong} of {calls} calls returned a stale generation"
    );
    check!(
        calls >= FLIPS / 8 && changes >= FLIPS / 8,
        "the runner barely ran: {calls} calls, {changes} generations seen"
    );
    Ok(())
}
