//! Kernel stacks: one per task, taken from the heap while the task exists.
//!
//! They used to be a static array with a slot each (`KSTACKS`), 12 MiB of
//! `.bss` at 256 slots and 48 MiB at 1024, paid on every boot whether or not a
//! task ever ran in the slot. A [`KernelStack`] is allocated when a slot is
//! claimed and freed with its [`Task`](super::Task), so an idle table costs
//! nothing and a machine's task ceiling (`limit.task_max`) is a RAM question,
//! not a link-time one.
//!
//! A stack is only handed out while the heap keeps a reserve ([`heap_reserve`]):
//! `clone` then fails cleanly with "out of memory" long before the kernel's
//! own allocations, which cannot fail softly, run into an exhausted heap.
//!
//! A stack is zeroed when it is taken: [`high_water`] measures how much of it
//! the task ever wrote (the lowest non-zero word), and a stale frame from a
//! previous owner of the memory must not read as depth.
//!
//! [`top_of`] and [`holder_of`] read a lock-free table of live stack tops, so
//! the fault reporter (which cannot take `TASKS`: the faulting code may hold
//! it) and [`super::kstack_headroom`] never touch the task table.

use alloc::alloc::{alloc_zeroed, dealloc, Layout};
use core::sync::atomic::{AtomicU64, Ordering};

use super::{Task, KSTACK_SIZE, MAX_TASKS};
use crate::limits;

/// Top (highest address) of each slot's live stack; 0 when the slot has none.
static TOPS: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];

/// 16-aligned so the top, where entry frames are built, satisfies the SysV
/// stack alignment the optimised kernel relies on.
const LAYOUT: Layout = match Layout::from_size_align(KSTACK_SIZE, 16) {
    Ok(layout) => layout,
    Err(_) => panic!("kernel stack layout"),
};

/// One task's kernel stack, owned by its [`Task`](super::Task).
pub struct KernelStack {
    slot: usize,
    base: *mut u8,
}

// SAFETY: the stack is plain memory owned by exactly one `KernelStack`; the
// raw pointer is only used to free it, and the address is handed to the CPU
// as a number (`top`), never dereferenced through this handle.
unsafe impl Send for KernelStack {}
// SAFETY: as above; the handle has no interior mutability.
unsafe impl Sync for KernelStack {}

impl KernelStack {
    /// A zeroed stack for `slot`, recorded as the slot's live one. `None`
    /// when the heap cannot supply [`KSTACK_SIZE`] bytes.
    pub fn new(slot: usize) -> Option<KernelStack> {
        if !heap_has_room() {
            return None;
        }
        // SAFETY: `LAYOUT` has a non-zero size.
        let base = unsafe { alloc_zeroed(LAYOUT) };
        if base.is_null() {
            return None;
        }
        let stack = KernelStack { slot, base };
        TOPS[slot].store(stack.top(), Ordering::Release);
        Some(stack)
    }

    /// Address just past the stack's last byte: where the first frame ends.
    pub fn top(&self) -> u64 {
        self.base as u64 + KSTACK_SIZE as u64
    }
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        // A slot reclaimed and reused while this handle was still being
        // dropped (after the table unlocks) keeps its new owner's entry.
        let _ =
            TOPS[self.slot].compare_exchange(self.top(), 0, Ordering::AcqRel, Ordering::Relaxed);
        // SAFETY: `base` came from `alloc_zeroed(LAYOUT)` and is freed once.
        unsafe { dealloc(self.base, LAYOUT) };
    }
}

/// Heap the kernel keeps for itself when it hands out stacks: its other
/// allocations cannot fail softly (a failed `Vec` growth is a panic), so a
/// flood of `clone`s must hit "no stack" while the heap still has this much
/// left, not exhaust it. The larger of 4 MiB and a sixteenth of `heap_max`.
fn heap_reserve(heap_max: usize) -> usize {
    (4 << 20).max(heap_max / 16)
}

/// Whether the heap can take a stack and still keep [`heap_reserve`] free:
/// what it has free, plus what it may still grow by.
fn heap_has_room() -> bool {
    let heap = crate::mem::heap_stats();
    let headroom = heap.max.saturating_sub(heap.total) + heap.free;
    headroom >= heap_reserve(heap.max) + KSTACK_SIZE
}

/// Top of `slot`'s live kernel stack, or 0 when it has none (the kernel task,
/// an empty slot).
pub fn top_of(slot: usize) -> u64 {
    TOPS.get(slot).map_or(0, |top| top.load(Ordering::Acquire))
}

/// The slot whose live kernel stack contains `addr`, as `(slot, top)`.
pub fn holder_of(addr: u64) -> Option<(usize, u64)> {
    TOPS.iter().enumerate().find_map(|(slot, top)| {
        let top = top.load(Ordering::Acquire);
        (top != 0 && addr >= top - KSTACK_SIZE as u64 && addr < top).then_some((slot, top))
    })
}

/// Bytes of `slot`'s live kernel stack that were ever written: the distance
/// from its top to the lowest non-zero word (0 when the slot has no stack).
/// A value equal to [`KSTACK_SIZE`] means the stack was used, or overflowed,
/// down to its last word.
pub fn high_water(slot: usize) -> u64 {
    let top = top_of(slot);
    if top == 0 {
        return 0;
    }
    let mut addr = top - KSTACK_SIZE as u64;
    while addr < top {
        // SAFETY: `addr` stays inside the live stack recorded for `slot`.
        if unsafe { core::ptr::read_volatile(addr as *const u64) } != 0 {
            return top - addr;
        }
        addr += 8;
    }
    0
}

/// Why a slot could not be claimed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimError {
    /// Every slot below `limit.task_max` is occupied.
    NoSlot,
    /// The heap could not supply a kernel stack.
    NoMemory,
}

impl ClaimError {
    pub fn message(self) -> &'static str {
        match self {
            ClaimError::NoSlot => "no free task slot",
            ClaimError::NoMemory => "out of memory (kernel stack)",
        }
    }
}

/// One bit per number that some task still names as its process group or
/// session (issue #880).
struct GroupNumbers([u64; MAX_TASKS.div_ceil(64)]);

impl GroupNumbers {
    fn of(tasks: &[Option<Task>; MAX_TASKS]) -> Self {
        let mut bits = GroupNumbers([0; MAX_TASKS.div_ceil(64)]);
        for task in tasks.iter().flatten() {
            bits.set(task.pgid);
            bits.set(task.sid);
        }
        bits
    }

    fn set(&mut self, number: usize) {
        if let Some(word) = self.0.get_mut(number / 64) {
            *word |= 1 << (number % 64);
        }
    }

    fn contains(&self, number: usize) -> bool {
        self.0
            .get(number / 64)
            .is_some_and(|word| word & (1 << (number % 64)) != 0)
    }
}

/// The lowest free slot below `limit.task_max`, with a kernel stack for it.
/// Callers take this first, before any other fallible step: a refusal then
/// has nothing to undo.
///
/// A slot is the pid of whatever runs in it, and a process group or a
/// session outlives its leader (its members keep `pgid == X` after the
/// leader in slot X is gone). Handing X out again would let its new holder
/// "form" group X with `setpgid(0, 0)` and silently join the old members, so
/// a slot whose number some task still uses as its pgid or sid is skipped,
/// as Linux's `alloc_pid` never returns a pid still in use as one (issue
/// #880). A group cannot outlive all of its members, so the slot comes back.
pub fn claim_slot(tasks: &[Option<Task>; MAX_TASKS]) -> Result<(usize, KernelStack), ClaimError> {
    let held = GroupNumbers::of(tasks);
    let index = (1..limits::task_max().min(MAX_TASKS))
        .find(|&slot| tasks[slot].is_none() && !held.contains(slot))
        .ok_or(ClaimError::NoSlot)?;
    let stack = KernelStack::new(index).ok_or(ClaimError::NoMemory)?;
    Ok((index, stack))
}

/// Slots below `limit.task_max` that are free (the kernel task's is not).
pub fn free_slots(tasks: &[Option<Task>; MAX_TASKS]) -> usize {
    tasks[1..limits::task_max().min(MAX_TASKS)]
        .iter()
        .filter(|slot| slot.is_none())
        .count()
}

/// How many slots hold a kernel stack right now (test builds: the leak check
/// of the spawn soaks).
#[cfg(lazyos_tests)]
pub fn live_count() -> usize {
    TOPS.iter()
        .filter(|top| top.load(Ordering::Acquire) != 0)
        .count()
}
