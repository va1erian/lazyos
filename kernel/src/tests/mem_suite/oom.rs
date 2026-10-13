//! Running out of frames in a page fault is an OOM kill, not a `SIGSEGV`
//! (issue #823).
//!
//! The harness cannot run user code, so these tests play the page fault
//! handler's part on a real address space: a Linux child with a VMA bigger
//! than the memory left, the same `demand_resolve` the handler calls until it
//! says `OutOfFrames`, then `mem::oom::kill`, which the handler calls next.
//! Nearly every frame is held back first (a [`Hoard`]) so exhaustion costs a
//! few hundred pages, not the machine's RAM, and the tests judge what a
//! program and the rest of the system see: the status `wait4` decodes, the
//! frames that come back, the tasks that live.

use super::*;
use crate::task::signal;
use crate::task::TaskState;

const PAGE: u64 = 4096;
/// Frames left for the victim to take before the allocator is empty.
const KEEP: usize = 96;
/// A range far larger than `KEEP` frames: touching it can only end one way.
const RANGE_PAGES: u64 = 8 * KEEP as u64;

/// Frames taken out of the allocator and given back on drop.
struct Hoard {
    frames: Vec<PhysAddr>,
}

impl Hoard {
    /// Take frames until `keep` are left free.
    fn leave(keep: usize) -> Result<Hoard, String> {
        // The vector's own storage comes from the heap, which may map frames:
        // size it first and count after.
        let room = mem::frame_stats().free;
        let mut frames = Vec::with_capacity(room);
        let mut hoard = Hoard { frames: Vec::new() };
        while mem::frame_stats().free > keep {
            let frame = mem::alloc_frame().ok_or("the allocator ran dry early")?;
            if frames.len() == room {
                mem::free_frame(frame);
                break;
            }
            frames.push(frame);
        }
        hoard.frames = frames;
        Ok(hoard)
    }
}

impl Drop for Hoard {
    fn drop(&mut self) {
        for frame in self.frames.drain(..) {
            mem::free_frame(frame);
        }
    }
}

fn fresh() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    signal::harness::reset();
    Ok(())
}

/// A child process with its own address space and an anonymous VMA of
/// [`RANGE_PAGES`] at the returned base.
fn victim() -> Result<(usize, PhysAddr, u64), String> {
    let elf = service_suite::minimal_elf();
    let slot = task::spawn_linux_child("oomvictim", &elf, &["oomvictim"])
        .map_err(|error| format!("spawn: {error}"))?;
    let table = PhysAddr::new(task::harness::pml4(slot).ok_or("the child has no address space")?);
    let base = process::linux::MMAP_BASE + 0x2000_0000;
    mem::vma::insert(
        table,
        base,
        base + RANGE_PAGES * PAGE,
        Prot::READ | Prot::WRITE,
        Kind::Anon,
    );
    Ok((slot, table, base))
}

/// Touch pages of the range, as the program would, until the handler's
/// resolver gives up. Returns how many pages were backed and the answer.
fn touch_until_refused(table: PhysAddr, base: u64) -> (u64, mem::Resolve) {
    for page in 0..RANGE_PAGES {
        let answer = mem::demand_resolve(
            table,
            base + page * PAGE,
            PageFaultErrorCode::CAUSED_BY_WRITE,
        );
        if answer != mem::Resolve::Handled {
            return (page, answer);
        }
    }
    (RANGE_PAGES, mem::Resolve::Handled)
}

/// One full round: spawn, starve, kill, reap. Returns the pages the victim
/// got before the allocator ran dry.
fn round(round: usize) -> Result<u64, String> {
    let (slot, table, base) = victim()?;
    let (touched, answer) = touch_until_refused(table, base);
    check!(
        answer == mem::Resolve::OutOfFrames,
        "round {round}: the resolver said {answer:?} after {touched} pages"
    );
    check!(
        touched > 0 && touched <= KEEP as u64,
        "round {round}: {touched} pages backed with {KEEP} frames free"
    );
    check!(
        mem::frame_stats().free == 0,
        "round {round}: {} frames still free",
        mem::frame_stats().free
    );

    // The page fault handler's next step, with the child as the faulting task.
    task::harness::switch_current(slot);
    let ended = mem::oom::kill(slot);
    task::harness::switch_current(task::KERNEL_TASK);
    check!(ended == 1, "round {round}: the kill ended {ended} tasks");
    check!(
        task::harness::state(slot) == Some(TaskState::Done),
        "round {round}: the victim is {:?}",
        task::harness::state(slot)
    );
    let (reaped, status) = task::reap_child().ok_or("the parent could not reap the victim")?;
    check!(
        reaped == slot && status == 128 + signal::SIGKILL as u64,
        "round {round}: reaped {reaped} with status {status}, wanted {slot}/137"
    );
    check!(
        mem::frame_stats().free >= touched as usize,
        "round {round}: {} frames free after the reap, {touched} were backed",
        mem::frame_stats().free
    );
    Ok(touched)
}

/// A process that touches more than the frames left is ended with `SIGKILL`
/// (status 137, which `wait4` reports as the signal), not `SIGSEGV` (139);
/// its frames come back and a sibling is untouched.
pub fn oom_kill_is_sigkill_and_returns_frames() -> Result<(), String> {
    fresh()?;
    let bystander = task::spawn_fork().map_err(|error| format!("spawn bystander: {error}"))?;
    let live = mem::frame_stats().live();
    let hoard = Hoard::leave(KEEP)?;
    let outcome = round(0);
    drop(hoard);
    outcome?;
    check!(
        task::harness::state(bystander) == Some(TaskState::Runnable),
        "the bystander is {:?}",
        task::harness::state(bystander)
    );
    task::harness::finish(bystander, 0);
    let _ = task::reap_child();
    let leaked = mem::frame_stats().live().saturating_sub(live);
    check!(leaked <= 16, "{leaked} frames leaked by one OOM kill");
    check!(
        mem::frame_stats().double_frees == 0 && mem::frame_stats().invalid_frees == 0,
        "the allocator saw bad frees: {:?}",
        mem::frame_stats()
    );
    task::harness::reset();
    Ok(())
}

/// The fault resolver keeps telling a refused access from a shortage: no VMA,
/// `PROT_NONE` and a present page are `Refused` even with no frame free, so
/// they stay `SIGSEGV`; a mapped page with no frame to back it is the shortage.
pub fn oom_refusals_stay_segv() -> Result<(), String> {
    fresh()?;
    let (slot, table, base) = victim()?;
    let none_at = base + RANGE_PAGES * PAGE + 16 * PAGE;
    mem::vma::insert(table, none_at, none_at + 4 * PAGE, Prot(0), Kind::Anon);
    let hoard = Hoard::leave(0)?;
    let write = PageFaultErrorCode::CAUSED_BY_WRITE;
    let cases = [
        ("unmapped", base - 64 * PAGE, write),
        ("PROT_NONE", none_at, PageFaultErrorCode::empty()),
        (
            "present page",
            base,
            PageFaultErrorCode::PROTECTION_VIOLATION,
        ),
    ];
    let mut failure = None;
    for (what, va, error) in cases {
        let answer = mem::demand_resolve(table, va, error);
        if answer != mem::Resolve::Refused {
            failure = Some(format!("{what}: {answer:?}, not Refused"));
        }
    }
    let starved = mem::demand_resolve(table, base, write);
    drop(hoard);
    task::harness::finish(slot, 0);
    let _ = task::reap_child();
    task::harness::reset();
    if let Some(failure) = failure {
        return Err(failure);
    }
    check!(
        starved == mem::Resolve::OutOfFrames,
        "a mapped page with no frame free: {starved:?}"
    );
    Ok(())
}

/// Soak: 300 spawn/starve/kill/reap generations with the allocator held at
/// the edge. Every one ends with `SIGKILL`, gets back what it took, and the
/// whole run leaks no frame, task slot or double free.
pub fn soak_oom_kill_generations() -> Result<(), String> {
    const ROUNDS: usize = 300;
    fresh()?;
    let tasks_in_use = || {
        (1..task::MAX_TASKS)
            .filter(|slot| task::harness::state(*slot).is_some())
            .count()
    };
    let hoard = Hoard::leave(KEEP)?;
    // One warm-up outside the measurement: slab caches and VMA lists settle.
    let warm = round(0);
    let live = mem::frame_stats().live();
    let tasks = tasks_in_use();
    let outcome = warm.and_then(|_| {
        for generation in 1..=ROUNDS {
            round(generation)?;
        }
        Ok(())
    });
    let live_after = mem::frame_stats().live();
    let tasks_after = tasks_in_use();
    drop(hoard);
    outcome?;
    check!(
        live_after <= live + 16,
        "frames in use grew from {live} to {live_after} over {ROUNDS} kills"
    );
    check!(
        tasks_after <= tasks,
        "task slots grew from {tasks} to {tasks_after}"
    );
    check!(
        mem::frame_stats().double_frees == 0 && mem::frame_stats().invalid_frees == 0,
        "the allocator saw bad frees: {:?}",
        mem::frame_stats()
    );
    task::harness::reset();
    Ok(())
}
