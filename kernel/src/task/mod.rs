//! Preemptive priority scheduling and ring-3 tasks.
//!
//! The timer ISR ([`switch::timer_isr`]) pushes the general-purpose registers,
//! calls [`schedule`], and resumes whatever `schedule` returns. Each task has
//! its own address space (PML4), kernel stack, and terminal (output buffer +
//! input queue).
//!
//! # Priority classes and fair share (issue #58)
//!
//! Every task has a [`PriorityClass`] and a weight. A scheduling decision
//! picks the highest class with a `Runnable` task (`Realtime` >
//! `Interactive` > `Normal` > `Background`), then the member of that class
//! with the smallest virtual pass. Running a quantum advances the pass by
//! `STRIDE_UNIT / weight` — stride scheduling — so tasks in one class share
//! the CPU in proportion to their weights, while equal passes (for example
//! freshly spawned tasks) rotate in round-robin order after the current task.
//!
//! Starvation bounds:
//!
//! * classes are strict, so `Background` work can never delay an
//!   `Interactive` task; a lower class only runs when no member of a higher
//!   class is runnable. `Realtime` is meant for short, latency-critical
//!   bursts: a runaway `Realtime` task can starve the classes below it, which
//!   is the documented trade-off of strict priority.
//! * within a class, between two selections of task `i` a peer `j` can be
//!   selected at most `ceil(stride_i / stride_j) + 1` times. With weights
//!   clamped to [`MIN_WEIGHT`]..=[`MAX_WEIGHT`] (1..=32) and at most
//!   `MAX_TASKS - 1` peers, a task waits under 2100 ticks — 21 seconds at the
//!   100 Hz timer — in the worst case (63 peers × 33 selections).
//! * a task that slept while its peers ran rejoins at the current virtual
//!   time ([`virtual_now`]) instead of claiming a backlog of catch-up quanta.
//!
//! The kernel task (slot 0, the multiplexer) is `Interactive` and competes
//! like any other task; it cannot starve user work because `mux::run` parks it
//! with [`idle`] between frames, so it is `Runnable` only for the quantum it
//! needs to repaint. Linux `getpriority`/`setpriority` are not wired to
//! [`set_priority`] yet: syscall dispatch lives in `crate::process::linux`,
//! outside this module, and should map nice values onto these classes when it
//! lands.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;
use x86_64::PhysAddr;

use crate::arch::gdt;
use crate::input::keyboard::Key;
use crate::ipc::credentials;
use crate::ipc::epoll::Epoll;
use crate::ipc::eventfd::EventFd;
use crate::ipc::inet::InetSock;
use crate::ipc::pipe::{self, End, Pipe, Side, SocketPair};
use crate::ipc::unix::Listener;
use crate::mem;
// `process` in this module is the process tree (`task::process`); the ELF
// loader and syscall shim live in `crate::process`, aliased here to keep the
// two apart.
use crate::process as user_process;

pub mod diag;
pub mod introspect;
mod linux_spawn;
pub mod process;
pub mod relax;
pub mod signal;
mod snapshot;
pub use snapshot::{
    fd_advance, fd_apply_pwrite, fd_apply_write, fd_file_meta, fd_offset, fd_peek, fd_peek_at,
    fd_read, fd_seek, fd_set_len, fd_size, prepare_fd_write, FileMeta, SnapFile,
};
pub mod switch;
pub mod sys;
pub mod trace;
pub mod wait;
#[cfg(lazyos_tests)]
pub use linux_spawn::spawn_linux_child;
pub use linux_spawn::{spawn_linux, spawn_linux_args, spawn_linux_child_env};

mod console;
pub mod consoletty;
mod cwd;
mod fdio;
mod fdops;
mod fdshare;
mod fdtable;
mod fdtypes;
pub mod fpu;
mod fs_base;
#[cfg(lazyos_tests)]
pub mod harness;
#[cfg(lazyos_tests)]
pub mod kthread;
mod lifecycle;
pub mod linuxstate;
mod memstate;
mod preempt;
mod sched;
mod schedule;
pub mod slotmask;
mod spawn;
mod stats;
mod waiting;

pub use console::*;
pub use cwd::{cwd, set_cwd};
pub use fdio::*;
pub use fdops::*;
pub use fdtable::{fd_max, FdTable};
pub use fdtypes::*;
pub use lifecycle::*;
pub use linuxstate::{LinuxExtras, ThreadShare};
pub use memstate::*;
#[allow(unused_imports)] // test hook
pub use preempt::pending as resched_pending;
pub use preempt::{exit_cpu, interrupted_quiet_context, preempt_point};
pub use sched::*;
pub use spawn::*;
pub use stats::*;
pub use waiting::*;

pub use fs_base::{set_fs_base, valid_fs_base};

/// Slots: 0 is the kernel (multiplexer), 1.. are user programs/threads.
///
/// 256 is a plain constant, not a design: the table, the kernel stacks
/// (`KSTACKS`, 12 MiB at this size) and every per-slot registry stay static
/// arrays, and the snapshot ABIs (`ipc::stats`, `sysinfo`, `task::introspect`)
/// carry one row per slot, so raising it bumps their versions (issue #204 took
/// it to 64; the application package system to 256, so a few dozen installed
/// apps and their helpers fit beside the boot services). Slot bitmasks use
/// [`slotmask::SlotMask`] and large per-slot snapshots live on the heap, since
/// a kernel stack is only [`KSTACK_SIZE`].
pub const MAX_TASKS: usize = 256;
/// Index of the kernel task.
pub const KERNEL_TASK: usize = 0;
/// Size of each task's kernel stack. 48 KiB since USB storage: a file write
/// that reaches a stick parks deep inside ext2 (`block::provider`), and the
/// scheduler's own frames (`schedule`, `signal::sweep`, 4 KiB each) then sit
/// on top of ext2's; 32 KiB overflowed there.
const KSTACK_SIZE: usize = 48 * 1024;
/// Number of qwords in a bootstrapped user frame (15 regs + RIP/CS/RFLAGS/RSP/SS).
const FRAME_WORDS: u64 = 20;

/// Which task currently owns the CPU.
static CURRENT: AtomicUsize = AtomicUsize::new(KERNEL_TASK);
/// The task that receives keyboard input.
static FOCUS: AtomicUsize = AtomicUsize::new(1);
/// Set when the screen needs repainting.
pub static NEEDS_REDRAW: AtomicBool = AtomicBool::new(true);
/// Bumped on every terminal input delivery; `epoll` edge-triggered interests
/// use it to tell a fresh key from a still-pending one.
static INPUT_GEN: AtomicU64 = AtomicU64::new(0);
/// True once the scheduler is running (changes how `exit` behaves).
static SCHEDULING: AtomicBool = AtomicBool::new(false);
/// Slots of finished parentless tasks waiting to be reclaimed from task
/// context (issue #133). The scheduler cannot free a task itself: a `Task` owns
/// heap buffers whose drop takes the heap lock, and the interrupted task may
/// hold that lock (the multiplexer clones window output with interrupts
/// enabled). `schedule` only sets a bit; [`reclaim_pending`] does the freeing
/// from a syscall entry or the mux loop, where the current task holds no lock.
static PENDING_RECLAIM: slotmask::SlotMask = slotmask::SlotMask::new();
/// Slots of finished tasks whose descriptors are still open. A process closes
/// its files when it exits, not when its parent reaps it: a shell reading a
/// `$(...)` substitution waits for the pipe's end-of-file before it reaps the
/// writer. [`process::finish_locked`] only sets a bit (it may run on the
/// scheduler's lock); [`close_exited_fds`] drops the descriptors.
static PENDING_CLOSE: slotmask::SlotMask = slotmask::SlotMask::new();

/// Which syscall ABI a task uses.
#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    /// LazyOS native `int 0x80` programs.
    Native,
    /// Linux `syscall`/`sysret` binaries.
    Linux,
}

/// Why a task is parked. Carried in [`TaskState::Blocked`] so a stuck task can
/// be told apart from a merely idle one (debugging, future `wait_queue_stats`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitKind {
    /// Waiting on a futex word.
    Futex,
    /// Waiting for terminal input.
    Terminal,
    /// Waiting for a child process to become reapable (`wait4`).
    ChildExit,
    /// Waiting for a `nanosleep` deadline (nothing notifies this queue).
    Sleep,
    /// Waiting on a pipe/socket event (data, space, EOF or `-EPIPE`).
    Pipe,
    /// Parked in `accept` on an `AF_UNIX` listener with no pending connection.
    UnixAccept,
    /// Waiting for one of a `poll` set to become ready.
    Poll,
    /// Parked by a stop signal (`SIGSTOP`/`SIGTSTP`/...). Only `SIGCONT`
    /// wakes a task in this state; other signals leave it stopped.
    Signal,
    /// Waiting for a task slot to become free (`clone` under table pressure).
    Slot,
    /// Waiting for a user-space block provider (`usbd`) to finish a request
    /// (`block::provider`), or the provider waiting for work.
    Block,
}

/// How a blocked task's wait ended. The wake path records it, the wait loop
/// consumes it, and blocking syscalls translate it to their ABI's error codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeReason {
    /// A wait queue was notified before the deadline.
    Woken,
    /// The PIT deadline passed before any notification.
    TimedOut,
    /// The task was interrupted by a deliverable signal (`task::signal`), so
    /// the blocking syscall returns `EINTR` and the signal is delivered on the
    /// way back to user mode.
    Interrupted,
}

/// What a task is doing between timer ticks.
///
/// Replaces the old `done`/`blocked` booleans: the scheduler only ever selects
/// [`TaskState::Runnable`], so a task cannot run while it is still parked, and
/// `Done` keeps the `wait4`/reap semantics (the slot is not freed until the
/// parent collects it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    /// Eligible for the scheduler.
    Runnable,
    /// Parked on a wait queue until woken or until `deadline` (absolute PIT
    /// ticks, 100 Hz) passes. `None` means no timeout.
    Blocked {
        wait: WaitKind,
        deadline: Option<u64>,
    },
    /// Finished; kept until its parent reaps it or restarts the slot.
    Done,
}

pub struct Task {
    pub name: &'static str,
    #[allow(dead_code)] // Kept for per-kind behaviour as the shim grows.
    pub kind: Kind,
    pub pml4: u64,
    pub kstack_top: u64,
    pub rsp: u64,
    /// Scheduler-visible state; see [`TaskState`].
    pub state: TaskState,
    /// Scheduling class (issue #58); see [`set_priority`].
    pub class: PriorityClass,
    /// Weight inside [`Task::class`]; see [`set_weight`].
    pub weight: u16,
    /// Stride-scheduler virtual pass: selection takes the runnable task with
    /// the smallest pass, which then advances by `stride(weight)`.
    pub pass: u64,
    /// CPU ticks (100 Hz PIT ticks) charged while this task was on the CPU.
    pub cpu_ticks: u64,
    /// How the current/last wait ended. Set by the wake path, consumed by the
    /// task's wait loop when it resumes. `None` while not waiting.
    pub wake_reason: Option<WakeReason>,
    /// Linux `clear_child_tid`: zeroed and futex-woken on thread exit.
    pub clear_child_tid: u64,
    /// Slot of the parent process (0 = none: kernel and threads).
    pub parent: usize,
    /// Process group id: the pid of the group leader (see [`process`]).
    pub pgid: usize,
    /// Session id: the pid of the session leader (see [`process`]).
    pub sid: usize,
    /// Exit status, valid once `done`.
    pub exit_status: u64,
    /// Native `sbrk` heap break.
    pub heap_break: u64,
    /// Linux thread pointer (`%fs` base).
    pub fs_base: u64,
    /// Linux file descriptors and their flags ([`FD_CLOEXEC`]).
    pub fds: FdTable,
    /// Absolute, normalized working directory of the Linux ABI (see [`cwd`]);
    /// `None` is the root, so a task that never `chdir`s costs no allocation.
    /// Shared by `Arc` so `fork` is a reference-count bump, and freed with
    /// the task, so no exit path has anything extra to release.
    pub cwd: Option<Arc<str>>,
    /// Linux-only state: share groups, thread group, exit signal, program
    /// path (see [`linuxstate`]).
    pub linux: LinuxExtras,
    pub output: Vec<u8>,
    pub input: VecDeque<Key>,
}

static TASKS: Mutex<[Option<Task>; MAX_TASKS]> = Mutex::new([const { None }; MAX_TASKS]);
/// One task's kernel stack. 16-aligned so the top (where entry frames are
/// built) satisfies the SysV stack alignment the optimised kernel relies on.
#[repr(align(16))]
#[allow(dead_code)] // the bytes are only ever used through the stack's address
struct KStack([u8; KSTACK_SIZE]);

static mut KSTACKS: [KStack; MAX_TASKS] = [const { KStack([0; KSTACK_SIZE]) }; MAX_TASKS];

/// Linux `brk`/`mmap` bump state, keyed by PML4 so threads share it.
#[derive(Clone, Copy)]
struct Bump {
    pml4: u64,
    /// Where the break started (the end of the image): `brk` never goes below.
    brk_start: u64,
    brk: u64,
    mmap_next: u64,
}

static BUMPS: Mutex<Vec<Bump>> = Mutex::new(Vec::new());

/// Register the shared bump state for a new address space whose break starts
/// (and currently is) at `brk`. Updates an existing entry as well: freed PML4
/// frames are recycled, so a stale entry must not leak into the new space.
pub fn register_bumps(pml4: u64, brk: u64, mmap_next: u64) {
    set_bumps(Bump {
        pml4,
        brk_start: brk,
        brk,
        mmap_next,
    });
}

/// Install `bump` as its address space's state (see [`register_bumps`]).
fn set_bumps(bump: Bump) {
    let mut bumps = BUMPS.lock();
    match bumps.iter_mut().find(|old| old.pml4 == bump.pml4) {
        Some(old) => *old = bump,
        None => bumps.push(bump),
    }
}

/// Drop the bump state of a torn-down address space.
fn forget_bumps(pml4: u64) {
    BUMPS.lock().retain(|bump| bump.pml4 != pml4);
}

/// Run `f` on the current address space's bump state.
fn with_bump<R>(f: impl FnOnce(&mut Bump) -> R) -> Option<R> {
    let pml4 = TASKS.lock()[current()].as_ref()?.pml4;
    let mut bumps = BUMPS.lock();
    bumps.iter_mut().find(|bump| bump.pml4 == pml4).map(f)
}

/// The bump state of a given address space, if it has one.
fn bump_for_pml4(pml4: u64) -> Option<Bump> {
    BUMPS.lock().iter().find(|bump| bump.pml4 == pml4).copied()
}

pub(crate) fn kstack_top(index: usize) -> u64 {
    // Safety: fixed-size static array.
    unsafe { (core::ptr::addr_of!(KSTACKS[index]) as u64) + KSTACK_SIZE as u64 }
}

/// Bytes left below the stack pointer on the current task's kernel stack, or
/// `None` when it is not running on one (the kernel task's boot stack).
pub(crate) fn kstack_headroom() -> Option<u64> {
    let slot = current();
    if slot == KERNEL_TASK || slot >= MAX_TASKS {
        return None;
    }
    let rsp: u64;
    // SAFETY: reads the stack pointer into a register; no memory is touched.
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack)) };
    let top = kstack_top(slot);
    let bottom = top - KSTACK_SIZE as u64;
    (bottom..=top).contains(&rsp).then(|| rsp - bottom)
}

/// Enable scheduling; call once the kernel task and user tasks are registered.
pub fn start() {
    SCHEDULING.store(true, Ordering::Relaxed);
}

/// The task currently on the CPU.
pub fn current() -> usize {
    CURRENT.load(Ordering::Relaxed)
}

/// The PML4 physical address of task `slot`'s address space, or `None` for an
/// empty slot.
///
/// This is the *slot's own* table, independent of whichever table is active on
/// the CPU right now: a syscall handler runs on its caller's table (so
/// `slot == current()` and the active CR3 agree), but a caller that charges or
/// releases another slot's resources must not assume that coincidence.
pub fn pml4_of(slot: usize) -> Option<u64> {
    TASKS.lock().get(slot)?.as_ref().map(|task| task.pml4)
}

/// The number of free task slots (the kernel task's slot is never free).
pub fn free_slots() -> usize {
    TASKS
        .lock()
        .iter()
        .skip(1)
        .filter(|slot| slot.is_none())
        .count()
}

/// The PIT tick counter (100 Hz). Wait deadlines are absolute tick values.
pub fn ticks() -> u64 {
    crate::arch::idt::TICKS.load(Ordering::Relaxed)
}

/// Timer ticks that found the CPU idle: the current task was parked in its
/// wait loop because nothing was runnable (see `schedule::charge_tick`).
/// `ticks() - idle_ticks()` is the CPU time charged to tasks.
pub static IDLE_TICKS: AtomicU64 = AtomicU64::new(0);

/// [`IDLE_TICKS`] as a value.
pub fn idle_ticks() -> u64 {
    IDLE_TICKS.load(Ordering::Relaxed)
}

/// The current task's `clear_child_tid` address.
pub fn clear_child_tid() -> u64 {
    TASKS.lock()[current()]
        .as_ref()
        .map(|task| task.clear_child_tid)
        .unwrap_or(0)
}

/// Set the current task's `clear_child_tid` address.
pub fn set_clear_child_tid(value: u64) {
    if let Some(task) = TASKS.lock()[current()].as_mut() {
        task.clear_child_tid = value;
    }
}
