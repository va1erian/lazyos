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
use crate::ipc::pipe::{self, End, Pipe, Side, SocketPair};
use crate::ipc::unix::Listener;
use crate::mem;
// `process` in this module is the process tree (`task::process`); the ELF
// loader and syscall shim live in `crate::process`, aliased here to keep the
// two apart.
use crate::process as user_process;

pub mod introspect;
pub mod process;
pub mod signal;
pub mod switch;
pub mod sys;
pub mod wait;

/// Slots: 0 is the kernel (multiplexer), 1.. are user programs/threads.
///
/// 64 is a plain constant, not a design: the table, the kernel stacks
/// (`KSTACKS`, 2 MiB at this size) and every per-slot registry stay static
/// arrays, and the snapshot ABIs (`ipc::stats`, `sysinfo`, `task::introspect`)
/// carry one row per slot, so raising it bumps their versions (issue #204).
pub const MAX_TASKS: usize = 64;
const _: () = assert!(
    MAX_TASKS <= u64::BITS as usize,
    "PENDING_RECLAIM is a u64 slot mask"
);
/// Index of the kernel task.
pub const KERNEL_TASK: usize = 0;
/// Size of each task's kernel stack.
const KSTACK_SIZE: usize = 32 * 1024;
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
static PENDING_RECLAIM: AtomicU64 = AtomicU64::new(0);

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

// ---------------------------------------------------------------------------
// Priority classes and stride scheduling (issue #58)
// ---------------------------------------------------------------------------

/// Scheduling class. Classes are strictly ordered, so a runnable task in a
/// higher class always beats every task in a lower one; inside one class the
/// stride scheduler shares the CPU by weight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PriorityClass {
    /// Batch work: runs only while nothing more important is runnable.
    Background,
    /// Default for Linux and native programs.
    Normal,
    /// Latency-sensitive work: shells, editors, and the kernel multiplexer.
    Interactive,
    /// Short, latency-critical bursts (audio, input). Strictly above
    /// `Interactive`; see the module docs for the starvation trade-off.
    Realtime,
}

impl PriorityClass {
    /// Every class, lowest first (tools and tests iterate this).
    pub const ALL: [PriorityClass; 4] = [
        PriorityClass::Background,
        PriorityClass::Normal,
        PriorityClass::Interactive,
        PriorityClass::Realtime,
    ];

    /// Position in the strict priority order (higher wins).
    const fn rank(self) -> u8 {
        match self {
            PriorityClass::Background => 0,
            PriorityClass::Normal => 1,
            PriorityClass::Interactive => 2,
            PriorityClass::Realtime => 3,
        }
    }

    /// Default weight of a task in this class. Only ratios inside one class
    /// matter (classes are strict), but the defaults grow with the class so a
    /// promoted task is also the heaviest member of its new class.
    pub const fn default_weight(self) -> u16 {
        match self {
            PriorityClass::Background => 1,
            PriorityClass::Normal => 2,
            PriorityClass::Interactive => 4,
            PriorityClass::Realtime => 8,
        }
    }

    /// Stable label for tools (`ps`, Task Manager, logs).
    #[allow(dead_code)] // used by the in-kernel tests until a `ps` tool lands
    pub const fn label(self) -> &'static str {
        match self {
            PriorityClass::Background => "background",
            PriorityClass::Normal => "normal",
            PriorityClass::Interactive => "interactive",
            PriorityClass::Realtime => "realtime",
        }
    }
}

/// Smallest assignable per-task weight.
pub const MIN_WEIGHT: u16 = 1;
/// Largest assignable per-task weight.
pub const MAX_WEIGHT: u16 = 32;
/// Virtual-time unit: a task of weight `w` pays `STRIDE_UNIT / w` of virtual
/// time per quantum, so selections follow the weight ratio.
const STRIDE_UNIT: u64 = 1024;
/// Passes are shifted back by their minimum once it reaches this mark (only
/// ordering matters, and small passes stay far from `u64` overflow).
const PASS_CEILING: u64 = 1 << 40;

/// The virtual time one quantum costs a task: its stride.
fn stride(weight: u16) -> u64 {
    (STRIDE_UNIT / weight.clamp(MIN_WEIGHT, MAX_WEIGHT) as u64).max(1)
}

/// The smallest pass among runnable tasks: the scheduler's "now". A task that
/// spawns or wakes here starts even with its peers instead of claiming a
/// backlog of catch-up quanta.
fn virtual_now(tasks: &[Option<Task>; MAX_TASKS]) -> u64 {
    tasks
        .iter()
        .flatten()
        .filter(|task| task.state == TaskState::Runnable)
        .map(|task| task.pass)
        .min()
        .unwrap_or(0)
}

/// The smallest pass in the whole table, blocked tasks included (a blocked
/// task's pass is its place in line when it wakes).
fn min_pass(tasks: &[Option<Task>; MAX_TASKS]) -> u64 {
    tasks
        .iter()
        .flatten()
        .map(|task| task.pass)
        .min()
        .unwrap_or(0)
}

/// Number of file descriptors per task.
pub const FD_COUNT: usize = 16;

/// Per-descriptor `FD_CLOEXEC` bit in [`Task::fd_flags`].
pub const FD_CLOEXEC: u16 = 1;

/// The socket type an unbound `socket(2)` descriptor carries to `connect`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SocketKind {
    Stream,
    Seqpacket,
}

/// A Linux file descriptor slot.
pub enum Fd {
    /// Unused slot.
    Closed,
    /// stdin/stdout/stderr (and `/dev/tty`): the task's own terminal.
    Terminal,
    /// A regular file: contents read at open time plus the current offset.
    File { data: Vec<u8>, offset: usize },
    /// One end of an anonymous pipe (`pipe`/`pipe2`).
    Pipe { pipe: Arc<Pipe>, end: End },
    /// One side of an `AF_UNIX` socket pair (`socketpair` or an accepted
    /// pathname connection).
    Socket { pair: Arc<SocketPair>, side: Side },
    /// An `eventfd` counter.
    Event { event: Arc<EventFd> },
    /// An `epoll` instance.
    Epoll { epoll: Arc<Epoll> },
    /// A bound, listening `AF_UNIX` socket.
    UnixListener { listener: Arc<Listener> },
    /// A socket created by `socket(2)` but not yet bound or connected.
    Unbound { kind: SocketKind, nonblock: bool },
}

impl Fd {
    /// A pipe end, taking the pipe's reader/writer reference.
    pub fn pipe_end(pipe: Arc<Pipe>, end: End) -> Fd {
        pipe.acquire(end);
        Fd::Pipe { pipe, end }
    }

    /// A socket side, taking the side's reference (and its directions' on the
    /// first open).
    pub fn socket_side(pair: Arc<SocketPair>, side: Side) -> Fd {
        pair.acquire(side);
        Fd::Socket { pair, side }
    }

    /// A socket side whose reference the caller already holds (a pending
    /// connection handed over by `Listener::take_pending`); nothing new is
    /// acquired, and dropping the `Fd` releases it.
    pub fn socket_side_adopt(pair: Arc<SocketPair>, side: Side) -> Fd {
        Fd::Socket { pair, side }
    }

    /// `poll` revents for this descriptor. `events` are `POLL*` bits; closed
    /// slots report nothing (callers map them to `POLLNVAL`).
    pub fn poll(&self, events: u16) -> u16 {
        self.poll_gen(events).0
    }

    /// [`poll`](Fd::poll) plus the handle's freshness counter, used by
    /// edge-triggered `epoll` interests.
    pub fn poll_gen(&self, events: u16) -> (u16, u64) {
        match self {
            Fd::Closed => (0, 0),
            Fd::Terminal => {
                // stdin's readiness is the shared input queue's; other
                // terminal descriptors are output-only.
                let mut revents = 0;
                if events & pipe::POLLIN != 0 && input_available() {
                    revents |= pipe::POLLIN;
                }
                if events & pipe::POLLOUT != 0 {
                    revents |= pipe::POLLOUT;
                }
                (revents, crate::task::input_gen())
            }
            Fd::File { .. } => {
                if events & pipe::POLLIN != 0 {
                    (pipe::POLLIN, 0)
                } else {
                    (0, 0)
                }
            }
            Fd::Pipe { pipe, end } => pipe.poll_gen(*end, events),
            Fd::Socket { pair, side } => pair.poll_gen(*side, events),
            Fd::Event { event } => event.poll_gen(events),
            Fd::Epoll { epoll } => {
                if events & pipe::POLLIN != 0 && epoll.has_ready() {
                    (pipe::POLLIN, 0)
                } else {
                    (0, 0)
                }
            }
            Fd::UnixListener { listener } => listener.poll_gen(events),
            Fd::Unbound { .. } => (0, 0),
        }
    }
}

/// Clone plus retain: `dup` and `fork` share the same pipe, so the reference
/// counts must follow the new descriptor.
impl Clone for Fd {
    fn clone(&self) -> Self {
        match self {
            Fd::Closed => Fd::Closed,
            Fd::Terminal => Fd::Terminal,
            Fd::File { data, offset } => Fd::File {
                data: data.clone(),
                offset: *offset,
            },
            Fd::Pipe { pipe, end } => Fd::pipe_end(Arc::clone(pipe), *end),
            Fd::Socket { pair, side } => Fd::socket_side(Arc::clone(pair), *side),
            Fd::Event { event } => Fd::Event {
                event: Arc::clone(event),
            },
            Fd::Epoll { epoll } => Fd::Epoll {
                epoll: Arc::clone(epoll),
            },
            Fd::UnixListener { listener } => Fd::UnixListener {
                listener: Arc::clone(listener),
            },
            Fd::Unbound { kind, nonblock } => Fd::Unbound {
                kind: *kind,
                nonblock: *nonblock,
            },
        }
    }
}

/// Releasing a descriptor drops its reference. This fires on `close`, on
/// `dup2` replacing a slot, and when a reaped task's table is dropped; the
/// drop must happen with the task table unlocked because the last reference
/// wakes a wait queue (queue-before-table lock order). The fd helpers below
/// take the old entry out under the lock and drop it after releasing it.
impl Drop for Fd {
    fn drop(&mut self) {
        match self {
            Fd::Pipe { pipe, end } => pipe.release(*end),
            Fd::Socket { pair, side } => pair.close(*side),
            _ => {}
        }
    }
}

/// Cheap classification of a descriptor for syscall dispatch.
#[derive(Clone, Copy, PartialEq)]
pub enum FdKind {
    Closed,
    Terminal,
    File,
    /// A pipe end (either direction).
    Pipe,
    /// A socket-pair side.
    Socket,
    /// An eventfd counter.
    EventFd,
    /// An epoll instance.
    Epoll,
    /// A bound `AF_UNIX` listener.
    Listener,
    /// A socket not yet bound or connected.
    Unbound,
}

fn new_fds() -> [Fd; FD_COUNT] {
    // 0/1/2 are the standard streams.
    core::array::from_fn(|i| if i < 3 { Fd::Terminal } else { Fd::Closed })
}

/// Copy a descriptor table (for `fork`; file buffers are duplicated, pipe
/// references retained).
fn clone_fds(fds: &[Fd; FD_COUNT]) -> [Fd; FD_COUNT] {
    core::array::from_fn(|i| fds[i].clone())
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
    /// Linux file descriptors.
    pub fds: [Fd; FD_COUNT],
    /// Per-descriptor flags ([`FD_CLOEXEC`]).
    pub fd_flags: [u16; FD_COUNT],
    pub output: Vec<u8>,
    pub input: VecDeque<Key>,
}

static TASKS: Mutex<[Option<Task>; MAX_TASKS]> = Mutex::new([const { None }; MAX_TASKS]);
static mut KSTACKS: [[u8; KSTACK_SIZE]; MAX_TASKS] = [[0; KSTACK_SIZE]; MAX_TASKS];

/// Linux `brk`/`mmap` bump state, keyed by PML4 so threads share it.
struct Bump {
    pml4: u64,
    brk: u64,
    mmap_next: u64,
}

static BUMPS: Mutex<Vec<Bump>> = Mutex::new(Vec::new());

/// Register the shared bump state for a new address space. Updates an existing
/// entry as well: freed PML4 frames are recycled, so a stale entry must not
/// leak into the new address space.
pub fn register_bumps(pml4: u64, brk: u64, mmap_next: u64) {
    let mut bumps = BUMPS.lock();
    match bumps.iter_mut().find(|bump| bump.pml4 == pml4) {
        Some(bump) => {
            bump.brk = brk;
            bump.mmap_next = mmap_next;
        }
        None => bumps.push(Bump {
            pml4,
            brk,
            mmap_next,
        }),
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

/// The `(brk, mmap_next)` of a given address space.
fn bump_for_pml4(pml4: u64) -> (u64, u64) {
    BUMPS
        .lock()
        .iter()
        .find(|bump| bump.pml4 == pml4)
        .map(|bump| (bump.brk, bump.mmap_next))
        .unwrap_or((0, 0))
}

fn kstack_top(index: usize) -> u64 {
    // Safety: fixed-size static array.
    unsafe { (core::ptr::addr_of!(KSTACKS[index]) as u64) + KSTACK_SIZE as u64 }
}

/// Register the kernel task (the multiplexer running in ring 0).
pub fn register_kernel() {
    let mut tasks = TASKS.lock();
    tasks[KERNEL_TASK] = Some(Task {
        name: "kernel",
        kind: Kind::Native,
        pml4: mem::kernel_table().as_u64(),
        kstack_top: 0,
        rsp: 0,
        state: TaskState::Runnable,
        // The multiplexer serves input and painting: an interactive workload.
        // `mux::run` parks it between frames, which bounds its CPU share.
        class: PriorityClass::Interactive,
        weight: PriorityClass::Interactive.default_weight(),
        pass: 0,
        cpu_ticks: 0,
        wake_reason: None,
        clear_child_tid: 0,
        parent: 0,
        pgid: KERNEL_TASK,
        sid: KERNEL_TASK,
        exit_status: 0,
        heap_break: 0,
        fs_base: 0,
        fds: new_fds(),
        fd_flags: [0; FD_COUNT],
        output: Vec::new(),
        input: VecDeque::new(),
    });
}
/// Create a user task from an ELF image. Returns its slot index.
///
/// The program is started by the kernel: it has no parent and leads its own
/// process group and session.
pub fn spawn(name: &'static str, elf: &[u8]) -> Result<usize, &'static str> {
    spawn_in_space(name, elf, None)
}

/// Create a user task that is a child of the calling task. Returns its slot.
///
/// This is the supervision primitive the userspace `init` (issue #93) builds
/// on: the child's `parent` names the supervisor, so its exit is reaped with
/// [`reap_child`] and wakes a [`wait_child_exit`] sleeper. The child inherits
/// the supervisor's process group and session (it is not a session leader),
/// exactly as a service started by `init` should be.
pub fn spawn_child(name: &'static str, elf: &[u8]) -> Result<usize, &'static str> {
    spawn_in_space(name, elf, Some(current()))
}

/// Shared implementation of [`spawn`] and [`spawn_child`]: load a native ELF
/// into a fresh address space and register it as a runnable task. `parent` is
/// `None` for a kernel-started program (its own group/session leader) or the
/// slot of the supervisor starting a child.
fn spawn_in_space(
    name: &'static str,
    elf: &[u8],
    parent: Option<usize>,
) -> Result<usize, &'static str> {
    let mut tasks = TASKS.lock();
    let index = (1..MAX_TASKS)
        .find(|&i| tasks[i].is_none())
        .ok_or("no free task slot")?;

    let pml4 = mem::new_user_table().ok_or("out of memory")?;
    let entry = match user_process::load_image(pml4, elf) {
        Ok(entry) => entry,
        Err(err) => {
            // A partially loaded image still owns its frames: release them.
            mem::free_user_table(pml4);
            return Err(err);
        }
    };

    let top = kstack_top(index);
    let rsp = build_user_frame(top, entry, user_process::USER_STACK_TOP - 16);
    let class = PriorityClass::Normal;
    let pass = virtual_now(&tasks);
    let (parent_slot, pgid, sid) = match parent {
        Some(parent) => {
            let parent_task = tasks[parent].as_ref().ok_or("no parent task")?;
            (parent, parent_task.pgid, parent_task.sid)
        }
        // A program started by the kernel leads its own group and session
        // (pid == pgid == sid); a supervised child inherits its supervisor's.
        None => (0, index, index),
    };

    // A supervised child starts with its supervisor's credentials (never the
    // root default), a kernel-started program with the root default: the slot
    // may still hold a dead task's identity.
    match parent {
        Some(parent) => credentials::inherit(parent, index),
        None => credentials::reset_for_task(index),
    }
    tasks[index] = Some(Task {
        name,
        kind: Kind::Native,
        pml4: pml4.as_u64(),
        kstack_top: top,
        rsp,
        state: TaskState::Runnable,
        class,
        weight: class.default_weight(),
        pass,
        cpu_ticks: 0,
        wake_reason: None,
        clear_child_tid: 0,
        parent: parent_slot,
        pgid,
        sid,
        exit_status: 0,
        heap_break: user_process::USER_HEAP_BASE,
        fs_base: 0,
        fds: new_fds(),
        fd_flags: [0; FD_COUNT],
        output: Vec::new(),
        input: VecDeque::new(),
    });
    Ok(index)
}

/// Create a Linux task from a static ELF image. Returns its slot index.
pub fn spawn_linux(name: &'static str, elf: &[u8], argv0: &str) -> Result<usize, &'static str> {
    let mut tasks = TASKS.lock();
    let index = (1..MAX_TASKS)
        .find(|&i| tasks[i].is_none())
        .ok_or("no free task slot")?;

    let pml4 = mem::new_user_table().ok_or("out of memory")?;
    let (entry, stack_top) = match user_process::linux::load(pml4, elf, argv0) {
        Ok(loaded) => loaded,
        Err(err) => {
            // A partially loaded image still owns its frames: release them.
            mem::free_user_table(pml4);
            return Err(err);
        }
    };

    let top = kstack_top(index);
    let rsp = build_user_frame(top, entry, stack_top);
    let class = PriorityClass::Normal;
    let pass = virtual_now(&tasks);

    // Started by the kernel: root, and never a dead task's stale identity.
    credentials::reset_for_task(index);
    tasks[index] = Some(Task {
        name,
        kind: Kind::Linux,
        pml4: pml4.as_u64(),
        kstack_top: top,
        rsp,
        state: TaskState::Runnable,
        class,
        weight: class.default_weight(),
        pass,
        cpu_ticks: 0,
        wake_reason: None,
        clear_child_tid: 0,
        parent: 0,
        // Top-level Linux programs are their own group and session leader.
        pgid: index,
        sid: index,
        exit_status: 0,
        heap_break: 0,
        fs_base: 0,
        fds: new_fds(),
        fd_flags: [0; FD_COUNT],
        output: Vec::new(),
        input: VecDeque::new(),
    });
    register_bumps(
        pml4.as_u64(),
        user_process::linux::BRK_BASE,
        user_process::linux::MMAP_BASE,
    );
    Ok(index)
}

/// Create a Linux thread that shares the current task's address space.
///
/// The child resumes at the caller's `syscall` return address with `rax = 0`,
/// on its own user stack (`user_rsp`) and its own `%fs` TLS (`fs_base`), as
/// `clone(CLONE_VM | ...)` requires.
pub fn spawn_thread(
    name: &'static str,
    user_rsp: u64,
    fs_base: u64,
    clear_child_tid: u64,
) -> Result<usize, &'static str> {
    let mut tasks = TASKS.lock();
    let index = (1..MAX_TASKS)
        .find(|&i| tasks[i].is_none())
        .ok_or("no free task slot")?;
    let parent = tasks[current()].as_ref().ok_or("no parent task")?;
    let pml4 = parent.pml4;
    // A thread stays in its process's group and session (#59: threads do not
    // get a new one), so only a process can create a group or session.
    let (pgid, sid) = (parent.pgid, parent.sid);
    // Threads inherit their creator's scheduling class and weight, like
    // Linux threads share a nice value.
    let (class, weight) = (parent.class, parent.weight);
    let context = crate::arch::linux::user_context();

    let top = kstack_top(index);
    let rsp = build_thread_frame(top, &context, user_rsp);
    let pass = virtual_now(&tasks);

    // A thread runs with its creator's credentials, not the slot's leftovers.
    credentials::inherit(current(), index);
    tasks[index] = Some(Task {
        name,
        kind: Kind::Linux,
        pml4,
        kstack_top: top,
        rsp,
        state: TaskState::Runnable,
        class,
        weight,
        pass,
        cpu_ticks: 0,
        wake_reason: None,
        clear_child_tid,
        parent: 0,
        pgid,
        sid,
        exit_status: 0,
        heap_break: 0,
        fs_base,
        fds: new_fds(),
        fd_flags: [0; FD_COUNT],
        output: Vec::new(),
        input: VecDeque::new(),
    });
    Ok(index)
}

/// Create a `clone(CLONE_VM)` child that is *not* a thread: the vfork child
/// musl's `posix_spawn` builds (`CLONE_VM|CLONE_VFORK|SIGCHLD`). It resumes at
/// the caller's `syscall` return address with `rax = 0` on `user_rsp`, after
/// `dup2`-ing stdio and `execve`-ing.
///
/// The address space is a copy-on-write clone, not a true shared table (a
/// vfork child on LazyOS would otherwise share the parent's `exit_group`
/// thread group, so its `_exit` fallback would kill the parent). The
/// posix_spawn child only reads the argument block before `execve`, and the
/// parent synchronises through musl's status pipe, so a clone is semantically
/// sufficient.
pub fn spawn_vfork(user_rsp: u64) -> Result<usize, &'static str> {
    spawn_fork_inner(Some(user_rsp))
}

/// Fork the current Linux process: a new task with a deep copy of its address
/// space. Returns the child's slot (the parent's `fork` result); the child's
/// frame resumes at the parent's return address with `rax = 0`.
pub fn spawn_fork() -> Result<usize, &'static str> {
    spawn_fork_inner(None)
}

/// Shared [`spawn_fork`]/[`spawn_vfork`] body. `user_rsp` overrides the
/// child's resume stack (`clone` provides one); `None` resumes on the parent's.
fn spawn_fork_inner(user_rsp: Option<u64>) -> Result<usize, &'static str> {
    let mut tasks = TASKS.lock();
    let index = (1..MAX_TASKS)
        .find(|&i| tasks[i].is_none())
        .ok_or("no free task slot")?;
    let parent_index = current();
    let parent = tasks[parent_index].as_ref().ok_or("no parent task")?;
    let pml4 = parent.pml4;
    let fs_base = parent.fs_base;
    // `fork` inherits the parent's process group and session. Forking *from
    // the kernel task* (only the test harness does) starts a fresh leader:
    // init has no session of its own to hand down.
    let (pgid, sid) = if parent_index == KERNEL_TASK {
        (index, index)
    } else {
        (parent.pgid, parent.sid)
    };
    // `fork` inherits the parent's scheduling class and weight, like Linux.
    let (class, weight) = (parent.class, parent.weight);
    let (brk, mmap_next) = bump_for_pml4(pml4);
    let context = crate::arch::linux::user_context();
    let fds = clone_fds(&parent.fds);
    // `fork` inherits the parent's `FD_CLOEXEC` flags (they are per-descriptor,
    // and `execve` in the child closes whatever they mark).
    let fd_flags = parent.fd_flags;
    let pass = virtual_now(&tasks);

    // `fork` is only valid inside a user address space: the kernel task's table
    // holds low-half bootloader mappings (framebuffer, boot data) that are not
    // ours to share or copy-on-write. Give the child a fresh table there (the
    // test harness forks from the kernel task to exercise bookkeeping).
    //
    // The test must go through the *slot*, not `mem::kernel_table()`: that
    // helper reports the active `CR3`, which inside the fork syscall is the
    // parent's table, so comparing against it made every fork take the
    // fresh-table path and left the child without the parent's pages.
    let child_table = if parent_index == KERNEL_TASK {
        mem::new_user_table()
    } else {
        mem::clone_user_table(PhysAddr::new(pml4))
    }
    .ok_or("out of memory (fork)")?;
    let top = kstack_top(index);
    let rsp = build_thread_frame(top, &context, user_rsp.unwrap_or(context.rsp));

    // `fork`/`vfork` children inherit the parent's credentials; the slot may
    // still hold a dead task's (possibly root) identity.
    credentials::inherit(parent_index, index);
    tasks[index] = Some(Task {
        name: "fork",
        kind: Kind::Linux,
        pml4: child_table.as_u64(),
        kstack_top: top,
        rsp,
        state: TaskState::Runnable,
        class,
        weight,
        pass,
        cpu_ticks: 0,
        wake_reason: None,
        clear_child_tid: 0,
        parent: parent_index,
        pgid,
        sid,
        exit_status: 0,
        heap_break: 0,
        fs_base,
        fds,
        fd_flags,
        output: Vec::new(),
        input: VecDeque::new(),
    });
    drop(tasks);

    register_bumps(child_table.as_u64(), brk, mmap_next);
    // POSIX `fork` inherits dispositions, the blocked mask and the alternate
    // stack; pending signals do not cross the fork.
    signal::fork_inherit(pml4, child_table.as_u64());
    Ok(index)
}

/// Lay out a thread's first ring-3 frame from the parent's saved user context:
/// same registers (but `rax = 0`, the child's return from `clone`), same RIP,
/// and the child's own stack pointer.
fn build_thread_frame(
    kstack_top: u64,
    ctx: &crate::arch::linux::UserContext,
    user_rsp: u64,
) -> u64 {
    let selectors = gdt::selectors();
    // Register order must match `timer_isr`'s pop order (r15 .. rax).
    let regs = [
        ctx.r15, ctx.r14, ctx.r13, ctx.r12, ctx.rflags, ctx.r10, ctx.r9, ctx.r8, ctx.rbp, ctx.rdi,
        ctx.rsi, ctx.rdx, ctx.rip, ctx.rbx, 0, // rax: the child sees clone() return 0
    ];
    let base = kstack_top - FRAME_WORDS * 8;
    // Safety: writing within this task's kernel stack.
    unsafe {
        let frame = base as *mut u64;
        for (i, value) in regs.iter().enumerate() {
            core::ptr::write_volatile(frame.add(i), *value);
        }
        core::ptr::write_volatile(frame.add(15), ctx.rip); // RIP (after syscall)
        core::ptr::write_volatile(frame.add(16), selectors.user_code as u64); // CS
        core::ptr::write_volatile(frame.add(17), ctx.rflags | 0x200); // RFLAGS (IF set)
        core::ptr::write_volatile(frame.add(18), user_rsp); // RSP
        core::ptr::write_volatile(frame.add(19), selectors.user_data as u64); // SS
    }
    base
}

/// Lay out a fresh ring-3 entry frame on a kernel stack and return its RSP.
///
/// Layout (low to high) matches `timer_isr`'s pop order: 15 general registers,
/// then RIP, CS, RFLAGS, RSP, SS.
fn build_user_frame(kstack_top: u64, entry: u64, user_rsp: u64) -> u64 {
    let selectors = gdt::selectors();
    let base = kstack_top - FRAME_WORDS * 8;
    // Safety: writing within this task's kernel stack.
    unsafe {
        let frame = base as *mut u64;
        for i in 0..15 {
            core::ptr::write_volatile(frame.add(i), 0); // general registers
        }
        core::ptr::write_volatile(frame.add(15), entry); // RIP
        core::ptr::write_volatile(frame.add(16), selectors.user_code as u64); // CS
        core::ptr::write_volatile(frame.add(17), 0x202); // RFLAGS (IF set)
        core::ptr::write_volatile(frame.add(18), user_rsp); // RSP
        core::ptr::write_volatile(frame.add(19), selectors.user_data as u64); // SS
    }
    base
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

/// Mark the current task finished with an exit status and re-parent its
/// children to the kernel/init task (see [`process::finish`]).
pub fn finish_current(code: u64) {
    process::finish(current(), code);
}

/// Finish every task that shares the current address space: Linux's
/// `exit_group`, which ends the *thread group* rather than one thread. Returns
/// the `clear_child_tid` addresses of threads that had one so the caller can
/// zero them and futex-wake any joiner (the Linux shim does that part).
pub fn exit_thread_group(status: u64) -> Vec<u64> {
    let pml4 = TASKS.lock()[current()].as_ref().map(|task| task.pml4);
    let Some(pml4) = pml4 else {
        return Vec::new();
    };
    let (slots, tids) = {
        let tasks = TASKS.lock();
        let mut slots = Vec::new();
        let mut tids = Vec::new();
        for (slot, task) in tasks.iter().enumerate() {
            let Some(task) = task else {
                continue;
            };
            if slot == KERNEL_TASK || task.pml4 != pml4 || task.state == TaskState::Done {
                continue;
            }
            slots.push(slot);
            if task.clear_child_tid != 0 {
                tids.push(task.clear_child_tid);
            }
        }
        (slots, tids)
    };
    for slot in slots {
        process::finish(slot, status);
    }
    tids
}

/// The current task's process group id.
pub fn pgid() -> usize {
    process::pgid_of(current())
}

/// The current task's parent pid (0 = the kernel/init task).
pub fn ppid() -> usize {
    process::ppid_of(current())
}

/// Terminate every task in process group `pgid` (issue #59). Kept for the test
/// harness; per-signal termination goes through `task::signal`.
#[allow(dead_code)]
pub fn kill_group(pgid: usize) -> usize {
    process::kill_group(pgid)
}

/// Whether the current task has any children.
pub fn has_children() -> bool {
    let tasks = TASKS.lock();
    let me = current();
    tasks
        .iter()
        .flatten()
        .any(|task| task.parent == me && task.parent != 0)
}

/// Drop an address space's non-task state and release its user pages, page
/// tables and PML4 frame. The caller guarantees no live task references
/// `pml4`.
fn release_address_space(pml4: u64) -> usize {
    // Give the per-uid user-memory charge this address space still holds back.
    crate::quota::forget_address_space(pml4);
    forget_bumps(pml4);
    signal::forget(pml4);
    mem::free_user_table(PhysAddr::new(pml4))
}

/// Remove `slot` if it holds a finished task that no `wait4` can ever collect:
/// a `clone(CLONE_VM)` thread or a kernel-started program has `parent == 0`,
/// so it has no reaper. Returns the removed task's address space and whether
/// another task still shares it (so the space must outlive this removal), or
/// `None` when nothing was removed.
fn take_finished(tasks: &mut [Option<Task>; MAX_TASKS], slot: usize) -> Option<(u64, bool)> {
    let task = tasks[slot].as_ref()?;
    if task.state != TaskState::Done || task.parent != 0 {
        return None;
    }
    let pml4 = task.pml4;
    // Dropping the task frees its fds and output/input buffers; the kernel
    // stack is a static array reused with the slot, so it needs no freeing.
    tasks[slot] = None;
    let shared = tasks
        .iter()
        .enumerate()
        .any(|(other, task)| other != slot && task.as_ref().is_some_and(|task| task.pml4 == pml4));
    Some((pml4, shared))
}

/// Flag `slot` for task-context reclamation if it holds a finished parentless
/// task. Called from the scheduler with the task table locked; the actual
/// freeing is deferred to [`reclaim_pending`] (see [`PENDING_RECLAIM`]).
fn mark_finished(tasks: &[Option<Task>; MAX_TASKS], slot: usize) {
    let finished_parentless = tasks[slot]
        .as_ref()
        .is_some_and(|task| task.state == TaskState::Done && task.parent == 0);
    if finished_parentless {
        PENDING_RECLAIM.fetch_or(1u64 << slot, Ordering::Relaxed);
    }
}

/// Reclaim the finished parentless tasks the scheduler flagged: free their
/// slots, task-owned buffers and — when the removal leaves an address space
/// with no users — its pages, page tables and PML4 frame.
///
/// Must run with interrupts disabled in task context (a syscall entry or the
/// mux loop): dropping a dead task takes the heap lock, and unlike a preempted
/// task the current task holds none inside a critical section there.
pub fn reclaim_pending() {
    let pending = PENDING_RECLAIM.swap(0, Ordering::Relaxed);
    if pending == 0 {
        return;
    }
    let mut tasks = TASKS.lock();
    let mut orphans = [0u64; MAX_TASKS];
    let mut orphan_count = 0;
    // (slot, its PML4, whether another task still shares that PML4).
    let mut removed = [(0usize, 0u64, false); MAX_TASKS];
    let mut removed_count = 0;
    for slot in 1..MAX_TASKS {
        if pending & (1u64 << slot) != 0 {
            if let Some((pml4, shared)) = take_finished(&mut tasks, slot) {
                removed[removed_count] = (slot, pml4, shared);
                removed_count += 1;
                if !shared {
                    orphans[orphan_count] = pml4;
                    orphan_count += 1;
                }
            }
        }
    }
    drop(tasks);
    // Release what the dead tasks still hold in the Messenger fabric before
    // their address spaces go away (`ipc::teardown_task` explains why the
    // order matters). The task table is unlocked: closing an endpoint wakes
    // waiters, which takes the wait-queue lock and then the table.
    for &(slot, pml4, shared) in &removed[..removed_count] {
        crate::ipc::teardown_task(slot, pml4, shared);
    }
    if removed_count > 0 {
        // A `clone` sleeping on table pressure can return early. Queue before
        // task table order holds: the lock above is already released.
        wait::SLOT.notify_all();
    }
    for &pml4 in &orphans[..orphan_count] {
        let pages = mem::user_table_frame_count(PhysAddr::new(pml4));
        let released = release_address_space(pml4);
        let stats = mem::frame_stats();
        serial_println!(
            "mem: reclaimed address space {pml4:#x}: {pages} pages, released {released} frames, {} free of {}",
            stats.free,
            stats.total
        );
    }
}

/// Take a finished child of the current task, freeing its slot and address
/// space. The address space is torn down only when the reaped child is its
/// last user: `clone(CLONE_VM)` threads share their creator's PML4 and would
/// otherwise be left with freed page tables.
///
/// The teardown runs after dropping the task lock: it is slow (and can lock
/// other state), while an interrupt here would otherwise self-deadlock on
/// `TASKS`.
pub fn reap_child() -> Option<(usize, u64)> {
    let me = current();
    let (index, status, pml4, shared, dead) = {
        let mut tasks = TASKS.lock();
        let mut found = None;
        for index in 1..MAX_TASKS {
            let finished = tasks[index]
                .as_ref()
                .map(|task| task.parent == me && task.state == TaskState::Done)
                .unwrap_or(false);
            if finished {
                // INVARIANT: `finished` was just computed from this same
                // `tasks[index]` under `TASKS.lock()`, held continuously
                // since; on today's single-CPU scheduler nothing else can
                // clear the slot in between. Revisit this unwrap if/when SMP
                // (platform-plan.md S8) lets another core touch `TASKS`
                // concurrently with a lock that isn't held for the whole
                // read-then-use span.
                let task = tasks[index].as_ref().unwrap();
                let status = task.exit_status;
                let pml4 = task.pml4;
                let shared = tasks.iter().enumerate().any(|(other, task)| {
                    other != index && task.as_ref().is_some_and(|task| task.pml4 == pml4)
                });
                let dead = tasks[index].take();
                found = Some((index, status, pml4, shared, dead));
                break;
            }
        }
        found?
    };
    // Dropping the dead task closes its descriptors, which may wake a peer
    // blocked on a pipe it held. Must happen with `TASKS` unlocked: pipe
    // release takes the wait-queue lock and then the task table.
    drop(dead);
    // The dead task's Messenger handles, buffers and mappings go before its
    // address space does.
    crate::ipc::teardown_task(index, pml4, shared);
    if !shared {
        let pages = mem::user_table_frame_count(PhysAddr::new(pml4));
        let released = release_address_space(pml4);
        let stats = mem::frame_stats();
        serial_println!(
            "mem: reaped task {index}: {pages} pages, released {released} frames, {} free of {}",
            stats.free,
            stats.total
        );
    }
    // A freed slot releases a `clone` sleeping on table pressure early.
    wait::SLOT.notify_all();
    Some((index, status))
}

/// Park the current task (skipped by the scheduler until woken).
///
/// Kept for the kernel test harness; real blocking goes through
/// [`wait::WaitQueue`], which also records a wake reason and a deadline.
#[allow(dead_code)]
pub fn set_blocked(blocked: bool) {
    if let Some(task) = TASKS.lock()[current()].as_mut() {
        if task.state == TaskState::Done {
            return;
        }
        task.state = if blocked {
            TaskState::Blocked {
                wait: WaitKind::Sleep,
                deadline: None,
            }
        } else {
            TaskState::Runnable
        };
        task.wake_reason = None;
    }
}

/// Whether the current task is parked on a wait queue.
///
/// Kept for the kernel test harness; see [`set_blocked`].
#[allow(dead_code)]
pub fn blocked() -> bool {
    TASKS.lock()[current()]
        .as_ref()
        .is_some_and(|task| matches!(task.state, TaskState::Blocked { .. }))
}

/// Wake a parked task by slot index, recording [`WakeReason::Woken`]. Returns
/// whether the task was actually parked.
///
/// Kept for the kernel test harness; wait queues use [`wake_task_with`].
#[allow(dead_code)]
pub fn wake_task(index: usize) -> bool {
    wake_task_with(index, WakeReason::Woken)
}

/// Mark task `index` blocked with a reason and an optional absolute deadline.
///
/// Callers park the task on a queue first and call this with interrupts
/// disabled, so the timer ISR can never schedule a half-parked task.
pub(crate) fn block_task(index: usize, wait: WaitKind, deadline: Option<u64>) {
    if let Some(task) = TASKS.lock()[index].as_mut() {
        if task.state != TaskState::Done {
            task.state = TaskState::Blocked { wait, deadline };
            task.wake_reason = None;
        }
    }
}

/// Move a blocked task back to `Runnable` and record why. Returns whether the
/// task was actually blocked (a task that already timed out, or is `Done`, is
/// left untouched so the scheduler never resurrects it).
pub(crate) fn wake_task_with(index: usize, reason: WakeReason) -> bool {
    let mut tasks = TASKS.lock();
    // A task that slept while its peers ran rejoins at the current virtual
    // time instead of being handed a burst of catch-up quanta (issue #58).
    let now = virtual_now(&tasks);
    if let Some(task) = tasks[index].as_mut() {
        if matches!(task.state, TaskState::Blocked { .. }) {
            task.state = TaskState::Runnable;
            task.wake_reason = Some(reason);
            task.pass = task.pass.max(now);
            return true;
        }
    }
    false
}

/// Consume the wake reason recorded for `index`, if any. The wait loop calls
/// this on resume; the reason is cleared so a later wait starts fresh.
pub(crate) fn take_wake_reason(index: usize) -> Option<WakeReason> {
    TASKS.lock()[index]
        .as_mut()
        .and_then(|task| task.wake_reason.take())
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

/// Set a task's scheduling class, resetting its weight to the class default.
/// Returns whether the slot holds a task.
///
/// This is the native/Linux-neutral priority API used by the kernel and tools;
/// Linux `nice`/`setpriority` are not routed here yet (see the module docs).
#[allow(dead_code)] // tool/test API; callers arrive with the scheduler features
pub fn set_priority(slot: usize, class: PriorityClass) -> bool {
    let mut tasks = TASKS.lock();
    match tasks.get_mut(slot).and_then(|task| task.as_mut()) {
        Some(task) => {
            task.class = class;
            task.weight = class.default_weight();
            true
        }
        None => false,
    }
}

/// A task's scheduling class, or `None` for an empty or invalid slot.
#[allow(dead_code)] // tool/test API; callers arrive with the scheduler features
pub fn priority(slot: usize) -> Option<PriorityClass> {
    TASKS.lock().get(slot)?.as_ref().map(|task| task.class)
}

/// Set a task's weight inside its class, clamped to
/// [`MIN_WEIGHT`]..=[`MAX_WEIGHT`]. Returns whether the slot holds a task.
#[allow(dead_code)] // tool/test API; callers arrive with the scheduler features
pub fn set_weight(slot: usize, weight: u16) -> bool {
    let mut tasks = TASKS.lock();
    match tasks.get_mut(slot).and_then(|task| task.as_mut()) {
        Some(task) => {
            task.weight = weight.clamp(MIN_WEIGHT, MAX_WEIGHT);
            true
        }
        None => false,
    }
}

/// A task's weight, or `None` for an empty or invalid slot.
#[allow(dead_code)] // tool/test API; callers arrive with the scheduler features
pub fn weight(slot: usize) -> Option<u16> {
    TASKS.lock().get(slot)?.as_ref().map(|task| task.weight)
}

/// One row of [`cpu_usage`]: a task's CPU accounting and scheduling class.
#[allow(dead_code)] // tool/test API; callers arrive with the scheduler features
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuUsage {
    pub slot: usize,
    pub name: &'static str,
    pub class: PriorityClass,
    pub weight: u16,
    pub state: TaskState,
    /// CPU ticks (100 Hz) charged while this task was on the CPU.
    pub ticks: u64,
}

/// Per-task CPU accounting, oldest slot first (the kernel task included).
/// Feeds tools (`ps`, Task Manager) and future CPU quotas (issue #58).
#[allow(dead_code)] // tool/test API; callers arrive with the scheduler features
pub fn cpu_usage() -> Vec<CpuUsage> {
    let tasks = TASKS.lock();
    tasks
        .iter()
        .enumerate()
        .filter_map(|(slot, task)| {
            task.as_ref().map(|task| CpuUsage {
                slot,
                name: task.name,
                class: task.class,
                weight: task.weight,
                state: task.state,
                ticks: task.cpu_ticks,
            })
        })
        .collect()
}

/// The CPU ticks charged to `slot` (0 for an empty or invalid slot).
#[allow(dead_code)] // tool/test API; callers arrive with the scheduler features
pub fn cpu_ticks(slot: usize) -> u64 {
    TASKS
        .lock()
        .get(slot)
        .and_then(|task| task.as_ref())
        .map(|task| task.cpu_ticks)
        .unwrap_or(0)
}

/// Park the current task until terminal input arrives.
pub fn wait_terminal() -> WakeReason {
    wait::TERMINAL.wait(current(), None)
}

/// Park the current task until terminal input or a pipe event arrives, or
/// `deadline` passes. The queue is advisory: the caller rescans its descriptors
/// and parks again if nothing it watches changed.
pub fn wait_poll(deadline: Option<u64>) -> WakeReason {
    wait::POLL.wait(current(), deadline)
}

/// Wake every `poll` waiter (pipe data, space, EOF, or `-EPIPE`). Pipe code and
/// the input paths call this; wakeups are advisory.
pub fn notify_poll() {
    wait::POLL.notify_all();
}

/// Park the current task until `deadline` (absolute PIT ticks) passes.
pub fn wait_sleep(deadline: u64) -> WakeReason {
    wait::SLEEP.wait(current(), Some(deadline))
}

/// Park the current task until `deadline` (absolute PIT ticks), from a context
/// with interrupts enabled: the multiplexer's between-frames idle primitive.
///
/// Unlike [`wait_sleep`] (called from syscalls that already run with
/// interrupts disabled), this disables them around the register-then-park
/// sequence itself and restores them before returning. Sleeping between
/// frames is what bounds the mux's CPU share: an `Interactive` task that is
/// only runnable one quantum in a handful cannot starve user work.
pub fn idle(deadline: u64) -> WakeReason {
    x86_64::instructions::interrupts::disable();
    let reason = wait::SLEEP.wait(current(), Some(deadline));
    x86_64::instructions::interrupts::enable();
    reason
}

/// Park the current task until one of its children becomes reapable.
pub fn wait_child_exit() -> WakeReason {
    wait::CHILD_EXIT.wait(current(), None)
}

/// Park the current task until a task slot is freed or `deadline` passes. The
/// Linux `clone` shim sleeps here after a spawn while the table is near
/// capacity, so earlier threads get a quantum to run, exit and free their
/// slots (which notifies the queue) before the next spawn needs one.
pub fn wait_slot(deadline: u64) -> WakeReason {
    wait::SLOT.wait(current(), Some(deadline))
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

/// Context switch: called from the timer ISR with the interrupted `rsp`.
///
/// Returns the `rsp` to resume (the next task's saved context).
#[no_mangle]
pub extern "C" fn schedule(current_rsp: u64) -> u64 {
    // Acknowledge the timer IRQ and keep a tick counter.
    crate::arch::idt::TICKS.fetch_add(1, Ordering::Relaxed);
    // Safety: we are in the timer IRQ handler.
    unsafe { crate::arch::pic::end_of_interrupt(0) };

    let mut tasks = TASKS.lock();
    let cur = CURRENT.load(Ordering::Relaxed);
    if let Some(task) = tasks[cur].as_mut() {
        task.rsp = current_rsp;
        // Charge the tick to the task that consumed it, so `cpu_usage` reports
        // real per-task CPU time even across ticks without a switch.
        task.cpu_ticks = task.cpu_ticks.saturating_add(1);
    }

    // Time out waiters whose deadline has passed. Doing it here, on the
    // scheduler's lock, means a timed-out task is runnable before this tick's
    // selection runs, and the wait path needs no separate timer callback.
    let now = crate::arch::idt::TICKS.load(Ordering::Relaxed);
    expire_deadlines(&mut tasks, now);

    // Apply pending signals at the boundary back to user mode: a handler frame
    // is written into the task's saved interrupt frame, a term/core default
    // marks the thread group Done. This is what reaches native `int 0x80`
    // programs, whose syscall stub is outside the signal layer. Terminations
    // are post-processed once the table lock is dropped.
    // Safety: every `Task::rsp` is an interrupt frame saved by this ISR.
    let (sweep_finished, sweep_count) = unsafe { signal::sweep(&mut tasks) };

    // Flag finished parentless tasks for reclamation: a thread or a
    // kernel-started program has no parent to `wait4` it, so its slot and
    // address space would otherwise leak (issue #133). The interrupted task is
    // left for the tick that switches away from it; `reclaim_pending` frees
    // the flagged slots from task context.
    for slot in 1..MAX_TASKS {
        if slot != cur {
            mark_finished(&tasks, slot);
        }
    }

    // Pick the highest class with a runnable task, then the fairest member
    // within it. A task that is blocked or done is never selected.
    // `select_next` falls back to `cur` when nothing is runnable at all;
    // resuming `cur` there just re-enters its wait loop instead of stalling
    // the CPU.
    let next = select_next(&mut tasks, cur);
    if next == cur {
        drop(tasks);
        signal::finish_sweep(&sweep_finished[..sweep_count]);
        return current_rsp;
    }

    // `cur` fully leaves the CPU on this tick: its finished slot can be
    // reclaimed too (from task context, on a later syscall or mux iteration).
    mark_finished(&tasks, cur);
    CURRENT.store(next, Ordering::Relaxed);
    // INVARIANT: `select_next` only ever returns an index whose slot is
    // `Some` (that is its definition of "runnable"), and `tasks` has been
    // locked continuously since it was called, so the slot cannot have been
    // cleared in between on today's single-CPU scheduler. Revisit if SMP
    // (platform-plan.md S8) introduces a window where another core can clear
    // a slot without holding this same lock across the whole span.
    let task = tasks[next].as_ref().unwrap();
    let (pml4, kstack_top, rsp, fs_base) = (task.pml4, task.kstack_top, task.rsp, task.fs_base);
    drop(tasks);
    signal::finish_sweep(&sweep_finished[..sweep_count]);

    // Switch address space and the ring0 stack used for the next user trap.
    mem::switch_to(PhysAddr::new(pml4));
    if kstack_top != 0 {
        gdt::set_kernel_stack(kstack_top);
        crate::arch::linux::set_kernel_stack(kstack_top);
    }
    // Restore this task's user thread pointer.
    crate::arch::msr::write(crate::arch::msr::IA32_FS_BASE, fs_base);
    rsp
}

/// Wake every blocked task whose absolute deadline has passed at `now`, with
/// [`WakeReason::TimedOut`]. The waiter is left enqueued: its wait loop removes
/// itself once it observes the reason, which keeps the queue and the task table
/// updates on their respective locks in a fixed order.
fn expire_deadlines(tasks: &mut [Option<Task>; MAX_TASKS], now: u64) {
    // Waking sleepers rejoin at the current virtual time, like queue wakeups:
    // a long sleep must not earn a burst of catch-up quanta (issue #58).
    let now_pass = virtual_now(tasks);
    for task in tasks.iter_mut().flatten() {
        if let TaskState::Blocked {
            deadline: Some(deadline),
            ..
        } = task.state
        {
            if now >= deadline {
                task.state = TaskState::Runnable;
                task.wake_reason = Some(WakeReason::TimedOut);
                task.pass = task.pass.max(now_pass);
            }
        }
    }
}

/// Whether `slot` is occupied and `Runnable` (blocked and done tasks are never
/// selected).
fn runnable(tasks: &[Option<Task>; MAX_TASKS], slot: usize) -> bool {
    tasks[slot]
        .as_ref()
        .is_some_and(|task| task.state == TaskState::Runnable)
}

/// The runnable task the stride scheduler would pick: the highest occupied
/// class, and inside it the smallest virtual pass. Ties (equal passes, e.g.
/// freshly spawned tasks) break in round-robin order after `cur`, so
/// equal-weight tasks rotate exactly like the old scheduler. Returns `None`
/// when nothing can run.
///
/// The kernel task competes like any other task. It cannot starve user work
/// because `mux::run` parks it with [`idle`] between frames: it is only
/// `Runnable` for the one quantum it needs to repaint, not all the time.
fn pick_next_best(tasks: &[Option<Task>; MAX_TASKS], cur: usize) -> Option<usize> {
    for rank in (0..PriorityClass::ALL.len()).rev() {
        let mut best: Option<(usize, u64)> = None;
        for step in 1..=MAX_TASKS {
            let slot = (cur + step) % MAX_TASKS;
            let Some(task) = tasks[slot].as_ref() else {
                continue;
            };
            if task.state != TaskState::Runnable || task.class.rank() as usize != rank {
                continue;
            }
            if best.is_none_or(|(_, pass)| task.pass < pass) {
                best = Some((slot, task.pass));
            }
        }
        if let Some((slot, _)) = best {
            return Some(slot);
        }
    }
    None
}

/// The scheduler's choice: [`pick_next_best`], or the interrupted task when
/// nothing is runnable at all so it can re-enter its wait loop instead of
/// stalling the CPU.
fn pick_next(tasks: &[Option<Task>; MAX_TASKS], cur: usize) -> usize {
    pick_next_best(tasks, cur).unwrap_or(cur)
}

/// [`pick_next`] plus stride accounting: the selected task pays one quantum
/// (its stride) of virtual time. Only a runnable winner is charged, so a
/// degenerate fallback to a parked `cur` does not advance its pass.
fn select_next(tasks: &mut [Option<Task>; MAX_TASKS], cur: usize) -> usize {
    let next = pick_next(tasks, cur);
    if runnable(tasks, next) {
        if let Some(task) = tasks[next].as_mut() {
            task.pass = task.pass.saturating_add(stride(task.weight));
        }
        renormalize(tasks);
    }
    next
}

/// Shift every pass back by the table minimum once it reaches
/// [`PASS_CEILING`]. Passes are only ever compared, so the shift is invisible
/// to selection while keeping the virtual clock far from `u64` overflow.
fn renormalize(tasks: &mut [Option<Task>; MAX_TASKS]) {
    let min = min_pass(tasks);
    if min < PASS_CEILING {
        return;
    }
    for task in tasks.iter_mut().flatten() {
        task.pass -= min;
    }
}

/// Append output to the current process's terminal, dropping ANSI escape
/// sequences (our window renderer has no terminal emulation yet). Forked
/// children write to their root ancestor's window.
pub fn write_output(bytes: &[u8]) {
    let mut tasks = TASKS.lock();
    let root = root_index(&tasks);
    if let Some(task) = tasks[root].as_mut() {
        strip_ansi(bytes, &mut task.output);
    }
    drop(tasks);
    NEEDS_REDRAW.store(true, Ordering::Relaxed);
}

/// Walk the parent chain to the process leader (the task with no parent).
fn root_index(tasks: &[Option<Task>; MAX_TASKS]) -> usize {
    let mut index = current();
    while let Some(task) = tasks[index].as_ref() {
        if task.parent == 0 {
            break;
        }
        index = task.parent;
    }
    index
}

/// Copy `bytes` into `out`, applying just enough terminal control for an
/// interactive shell: `\r`, backspace, erase-to-end-of-line, cursor-left, and
/// dropping other CSI sequences.
fn strip_ansi(bytes: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            i += 2;
            let start = i;
            while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                i += 1;
            }
            if i < bytes.len() {
                let count = parse_count(&bytes[start..i]);
                match bytes[i] {
                    b'K' => truncate_line(out),
                    b'D' => {
                        for _ in 0..count {
                            out.pop();
                        }
                    }
                    b'J' if count == 2 => out.clear(),
                    _ => {}
                }
                i += 1; // final byte
            }
            continue;
        }
        match byte {
            b'\r' => truncate_line(out),
            0x08 => {
                out.pop();
            }
            _ => out.push(byte),
        }
        i += 1;
    }
}

/// Parse a CSI parameter (defaults to 1 when empty).
fn parse_count(digits: &[u8]) -> usize {
    let mut value = 0usize;
    let mut any = false;
    for &d in digits {
        if d.is_ascii_digit() {
            value = value * 10 + (d - b'0') as usize;
            any = true;
        }
    }
    if any {
        value
    } else {
        1
    }
}

/// Discard the current (last) line's contents.
fn truncate_line(out: &mut Vec<u8>) {
    match out.iter().rposition(|&c| c == b'\n') {
        Some(pos) => out.truncate(pos + 1),
        None => out.clear(),
    }
}

/// Pop a key for the current process (its root ancestor's queue).
pub fn take_key() -> Option<Key> {
    let mut tasks = TASKS.lock();
    let root = root_index(&tasks);
    tasks[root].as_mut().and_then(|task| task.input.pop_front())
}

/// Route a decoded key: Tab cycles focus, others go to the focused task.
pub fn on_key(key: Key) {
    if key == Key::Tab {
        cycle_focus();
        return;
    }
    // Emulate the terminal line discipline's INTR character. There is no tty
    // layer to signal the foreground group, so Ctrl-C (ETX) is intercepted here
    // and becomes SIGINT for the focused task's process group. This is what
    // lets BusyBox `sh` interrupt a running child.
    if key == Key::Char('\u{3}') {
        let pgid = process::pgid_of(FOCUS.load(Ordering::Relaxed));
        let _ = signal::kill(
            KERNEL_TASK,
            -(pgid as i64),
            signal::SIGINT,
            signal::SigInfo::kernel(),
        );
        return;
    }
    let focus = FOCUS.load(Ordering::Relaxed);
    {
        let mut tasks = TASKS.lock();
        if let Some(task) = tasks[focus].as_mut() {
            task.input.push_back(key);
        }
    }
    // Wake blocked readers after releasing the task table: wait queues take the
    // task table inside notify, so the lock order is always queue -> task.
    // Readers that got no key just park again (spurious wakeup).
    INPUT_GEN.fetch_add(1, Ordering::AcqRel);
    wait::TERMINAL.notify_all();
    notify_poll();
}

/// Inject bytes into the current process's input queue (e.g. a terminal reply).
pub fn inject_input(bytes: &[u8]) {
    {
        let mut tasks = TASKS.lock();
        let root = root_index(&tasks);
        if let Some(task) = tasks[root].as_mut() {
            for &byte in bytes {
                task.input.push_back(Key::Char(byte as char));
            }
        }
    }
    INPUT_GEN.fetch_add(1, Ordering::AcqRel);
    wait::TERMINAL.notify_all();
    notify_poll();
}

/// Whether the current process has pending terminal input.
pub fn input_available() -> bool {
    let tasks = TASKS.lock();
    let root = root_index(&tasks);
    tasks[root]
        .as_ref()
        .map(|task| !task.input.is_empty())
        .unwrap_or(false)
}

/// Freshness counter for terminal input (see [`Fd::poll_gen`]).
pub fn input_gen() -> u64 {
    INPUT_GEN.load(Ordering::Acquire)
}

fn cycle_focus() {
    let tasks = TASKS.lock();
    let start = FOCUS.load(Ordering::Relaxed);
    for step in 1..=MAX_TASKS {
        let candidate = (start + step) % MAX_TASKS;
        if candidate == KERNEL_TASK {
            continue;
        }
        if let Some(task) = tasks[candidate].as_ref() {
            if task.state != TaskState::Done {
                FOCUS.store(candidate, Ordering::Relaxed);
                NEEDS_REDRAW.store(true, Ordering::Relaxed);
                return;
            }
        }
    }
}

/// The focused task index.
pub fn focus() -> usize {
    FOCUS.load(Ordering::Relaxed)
}

/// The current task's heap break (for `sbrk`).
pub fn heap_break() -> u64 {
    let tasks = TASKS.lock();
    tasks[current()].as_ref().map(|t| t.heap_break).unwrap_or(0)
}

/// Set the current task's heap break.
pub fn set_heap_break(value: u64) {
    let mut tasks = TASKS.lock();
    if let Some(task) = tasks[current()].as_mut() {
        task.heap_break = value;
    }
}

/// Set the current task's address space (used by `execve`).
///
/// When no other task references the previous table it is torn down here,
/// which closes the fork+exec leak. `execve` switches `CR3` before calling
/// this, so the old table is inactive. Tables still shared with
/// `clone(CLONE_VM)` threads are left alone; those threads currently have no
/// teardown path of their own.
pub fn set_pml4(value: u64) {
    let mut tasks = TASKS.lock();
    let me = current();
    let old = tasks[me].as_ref().map(|task| task.pml4);
    if let Some(task) = tasks[me].as_mut() {
        task.pml4 = value;
    }
    let orphaned = old.filter(|old| {
        *old != value
            && !tasks.iter().enumerate().any(|(other, task)| {
                other != me && task.as_ref().is_some_and(|task| task.pml4 == *old)
            })
    });
    drop(tasks);
    if let Some(old) = orphaned {
        // Guard against freeing whatever `CR3` currently points at (only
        // possible if `execve` were preempted between its switch and here).
        if mem::kernel_table().as_u64() == old {
            serial_println!("mem: not freeing active page table {old:#x}");
            return;
        }
        forget_bumps(old);
        // `execve` starts a fresh signal disposition table (Linux keeps SIG_IGN
        // but resets handlers; a fresh table means ignored ones reset too).
        signal::forget(old);
        let released = mem::free_user_table(PhysAddr::new(old));
        let stats = mem::frame_stats();
        serial_println!(
            "mem: execve released {released} frames, {} free of {}",
            stats.free,
            stats.total
        );
    }
}

/// The current task's `brk` break.
pub fn brk() -> u64 {
    with_bump(|bump| bump.brk).unwrap_or(0)
}

/// Set the current task's Linux `brk` break.
pub fn set_brk(value: u64) {
    let _ = with_bump(|bump| bump.brk = value);
}

/// The current address space's anonymous `mmap` bump pointer.
pub fn mmap_next() -> u64 {
    with_bump(|bump| bump.mmap_next).unwrap_or(0)
}

/// Set the current address space's anonymous `mmap` bump pointer.
pub fn set_mmap_next(value: u64) {
    let _ = with_bump(|bump| bump.mmap_next = value);
}

/// Set the current task's user thread pointer (`%fs` base), programming the CPU.
pub fn set_fs_base(value: u64) {
    if let Some(task) = TASKS.lock()[current()].as_mut() {
        task.fs_base = value;
    }
    crate::arch::msr::write(crate::arch::msr::IA32_FS_BASE, value);
}

/// Allocate the lowest free descriptor (>= 3) for `entry`.
pub fn fd_open(entry: Fd) -> Option<usize> {
    let mut tasks = TASKS.lock();
    let task = tasks[current()].as_mut()?;
    for index in 3..FD_COUNT {
        if matches!(task.fds[index], Fd::Closed) {
            task.fds[index] = entry;
            task.fd_flags[index] = 0;
            return Some(index);
        }
    }
    None
}

/// Replace an open descriptor's entry, returning false for a closed slot. The
/// old entry is returned for unlocked dropping by the caller (`bind` upgrades
/// an unbound socket, `connect` a connected one).
pub fn fd_replace(fd: usize, entry: Fd) -> Result<Fd, ()> {
    let mut tasks = TASKS.lock();
    let Some(task) = tasks[current()].as_mut() else {
        return Err(());
    };
    if fd >= FD_COUNT || matches!(task.fds[fd], Fd::Closed) {
        return Err(());
    }
    Ok(core::mem::replace(&mut task.fds[fd], entry))
}

/// Clone an open descriptor's entry, or `None` for a closed slot.
pub fn fd_clone(fd: usize) -> Option<Fd> {
    let tasks = TASKS.lock();
    let task = tasks[current()].as_ref()?;
    if fd >= FD_COUNT {
        return None;
    }
    match task.fds[fd] {
        Fd::Closed => None,
        _ => Some(task.fds[fd].clone()),
    }
}

/// Close a descriptor. The old entry is dropped after the task table is
/// unlocked: dropping a pipe end wakes its peer, and wait-queue notification
/// takes the task table (queue-before-table lock order). Any epoll instance in
/// this task that registered the descriptor drops the interest too, so a
/// reused descriptor number cannot inherit a stale registration.
pub fn fd_close(fd: usize) -> bool {
    let (old, epolls) = {
        let mut tasks = TASKS.lock();
        match tasks[current()].as_mut() {
            Some(task) if fd < FD_COUNT && !matches!(task.fds[fd], Fd::Closed) => {
                task.fd_flags[fd] = 0;
                let epolls: Vec<Arc<Epoll>> = task
                    .fds
                    .iter()
                    .filter_map(|entry| match entry {
                        Fd::Epoll { epoll } => Some(Arc::clone(epoll)),
                        _ => None,
                    })
                    .collect();
                (
                    Some(core::mem::replace(&mut task.fds[fd], Fd::Closed)),
                    epolls,
                )
            }
            _ => (None, Vec::new()),
        }
    };
    for epoll in &epolls {
        Epoll::drop_fd(epoll, fd);
    }
    let closed = old.is_some();
    drop(old);
    closed
}

/// Classify a descriptor.
pub fn fd_kind(fd: usize) -> FdKind {
    let tasks = TASKS.lock();
    match tasks[current()].as_ref() {
        Some(task) if fd < FD_COUNT => match task.fds[fd] {
            Fd::Closed => FdKind::Closed,
            Fd::Terminal => FdKind::Terminal,
            Fd::File { .. } => FdKind::File,
            Fd::Pipe { .. } => FdKind::Pipe,
            Fd::Socket { .. } => FdKind::Socket,
            Fd::Event { .. } => FdKind::EventFd,
            Fd::Epoll { .. } => FdKind::Epoll,
            Fd::UnixListener { .. } => FdKind::Listener,
            Fd::Unbound { .. } => FdKind::Unbound,
        },
        _ => FdKind::Closed,
    }
}

/// Whether `fd` has `FD_CLOEXEC` set (false for a closed slot).
pub fn fd_cloexec(fd: usize) -> bool {
    let tasks = TASKS.lock();
    match tasks[current()].as_ref() {
        Some(task) if fd < FD_COUNT && !matches!(task.fds[fd], Fd::Closed) => {
            task.fd_flags[fd] & FD_CLOEXEC != 0
        }
        _ => false,
    }
}

/// Set or clear `FD_CLOEXEC` on `fd`; `false` for a closed slot.
pub fn fd_set_cloexec(fd: usize, on: bool) -> bool {
    let mut tasks = TASKS.lock();
    match tasks[current()].as_mut() {
        Some(task) if fd < FD_COUNT && !matches!(task.fds[fd], Fd::Closed) => {
            if on {
                task.fd_flags[fd] |= FD_CLOEXEC;
            } else {
                task.fd_flags[fd] &= !FD_CLOEXEC;
            }
            true
        }
        _ => false,
    }
}

/// Close every descriptor marked `FD_CLOEXEC` (the `execve` step). Returns how
/// many were closed.
pub fn fd_close_cloexec() -> usize {
    let mut closed = 0;
    for fd in 0..FD_COUNT {
        if fd_cloexec(fd) && fd_close(fd) {
            closed += 1;
        }
    }
    closed
}

/// Linux `O_NONBLOCK` (as `fd_status`/`fd_set_status` carry it).
pub const O_NONBLOCK: u64 = 0o4000;

/// The access-mode bits of `F_GETFL` for `fd`, or `None` for a closed slot.
/// `O_NONBLOCK` is reported from the pipe/socket's open file description, so a
/// `dup` or `fork` sees the same setting.
pub fn fd_status(fd: usize) -> Option<u64> {
    let tasks = TASKS.lock();
    let task = tasks[current()].as_ref()?;
    if fd >= FD_COUNT {
        return None;
    }
    match &task.fds[fd] {
        Fd::Closed => None,
        Fd::Terminal | Fd::File { .. } => Some(0), // O_RDONLY
        Fd::Pipe { pipe, end } => {
            let access = match end {
                End::Read => 0,
                End::Write => 1, // O_WRONLY
            };
            Some(access | (u64::from(pipe.nonblock(*end)) * O_NONBLOCK))
        }
        Fd::Socket { pair, side } => {
            Some(2 | (u64::from(pair.nonblock(*side)) * O_NONBLOCK)) // O_RDWR
        }
        Fd::Event { event } => Some(2 | (u64::from(event.nonblock()) * O_NONBLOCK)),
        Fd::Epoll { epoll } => Some(2 | (u64::from(epoll.nonblock()) * O_NONBLOCK)),
        Fd::UnixListener { listener } => Some(2 | (u64::from(listener.nonblock()) * O_NONBLOCK)),
        Fd::Unbound { nonblock: flag, .. } => Some(2 | (u64::from(*flag) * O_NONBLOCK)),
    }
}

/// Apply `F_SETFL`: only `O_NONBLOCK` is meaningful (pipes, sockets, eventfds,
/// epolls and listeners); other status flags are accepted and ignored. `false`
/// for a closed slot.
pub fn fd_set_status(fd: usize, nonblock: bool) -> bool {
    let mut tasks = TASKS.lock();
    let Some(task) = tasks[current()].as_mut() else {
        return false;
    };
    if fd >= FD_COUNT {
        return false;
    }
    match &mut task.fds[fd] {
        Fd::Closed => false,
        Fd::Terminal | Fd::File { .. } => true,
        Fd::Pipe { pipe, end } => {
            pipe.set_nonblock(*end, nonblock);
            true
        }
        Fd::Socket { pair, side } => {
            pair.set_nonblock(*side, nonblock);
            true
        }
        Fd::Event { event } => {
            event.set_nonblock(nonblock);
            true
        }
        Fd::Epoll { epoll } => {
            epoll.set_nonblock(nonblock);
            true
        }
        Fd::UnixListener { listener } => {
            listener.set_nonblock(nonblock);
            true
        }
        Fd::Unbound { nonblock: flag, .. } => {
            *flag = nonblock;
            true
        }
    }
}

/// Read from a pipe end or socket side into kernel memory. The fd wrapper
/// resolves the shared object, drops the task-table lock, and then runs the
/// blocking read (which may park this task).
pub fn fd_stream_read(fd: usize, dst: &mut [u8]) -> Result<usize, pipe::Error> {
    enum Source {
        Pipe(Arc<Pipe>, End),
        Socket(Arc<SocketPair>, Side),
    }
    let (source, nonblock) = {
        let tasks = TASKS.lock();
        let task = tasks[current()].as_ref().ok_or(pipe::Error::BadEnd)?;
        if fd >= FD_COUNT {
            return Err(pipe::Error::BadEnd);
        }
        match &task.fds[fd] {
            Fd::Pipe { pipe, end } => (Source::Pipe(Arc::clone(pipe), *end), pipe.nonblock(*end)),
            Fd::Socket { pair, side } => (
                Source::Socket(Arc::clone(pair), *side),
                pair.nonblock(*side),
            ),
            _ => return Err(pipe::Error::BadEnd),
        }
    };
    match source {
        Source::Pipe(pipe, end) => pipe.read(end, dst, nonblock),
        Source::Socket(pair, side) => pair.read(side, dst, nonblock),
    }
}

/// Write to a pipe end or socket side from kernel memory.
pub fn fd_stream_write(fd: usize, src: &[u8]) -> Result<usize, pipe::Error> {
    enum Sink {
        Pipe(Arc<Pipe>, End),
        Socket(Arc<SocketPair>, Side),
    }
    let (sink, nonblock) = {
        let tasks = TASKS.lock();
        let task = tasks[current()].as_ref().ok_or(pipe::Error::BadEnd)?;
        if fd >= FD_COUNT {
            return Err(pipe::Error::BadEnd);
        }
        match &task.fds[fd] {
            Fd::Pipe { pipe, end } => (Sink::Pipe(Arc::clone(pipe), *end), pipe.nonblock(*end)),
            Fd::Socket { pair, side } => {
                (Sink::Socket(Arc::clone(pair), *side), pair.nonblock(*side))
            }
            _ => return Err(pipe::Error::BadEnd),
        }
    };
    match sink {
        Sink::Pipe(pipe, end) => pipe.write(src, end, nonblock),
        Sink::Socket(pair, side) => pair.write(side, src, nonblock),
    }
}

/// `poll` revents for a descriptor: `POLLIN`/`POLLOUT`/`POLLHUP` for streams,
/// terminal input readiness for fd 0. `None` means the slot is not open
/// (`POLLNVAL`).
pub fn fd_poll(fd: usize, events: u16) -> Option<u16> {
    // stdin's readiness comes from the input queue; `input_available` is the
    // one predicate for it (no table lock held here, so it can take its own).
    if fd == 0 && fd_kind(0) == FdKind::Terminal {
        return Some(Fd::Terminal.poll(events));
    }
    let target = fd_clone(fd)?;
    Some(target.poll(events))
}

/// Read the counter from an `eventfd` descriptor.
pub fn fd_eventfd_read(fd: usize) -> Result<u64, pipe::Error> {
    let target = fd_clone(fd).ok_or(pipe::Error::BadEnd)?;
    match &target {
        Fd::Event { event } => event.read(),
        _ => Err(pipe::Error::BadEnd),
    }
}

/// Add to an `eventfd` descriptor's counter.
pub fn fd_eventfd_write(fd: usize, value: u64) -> Result<(), pipe::Error> {
    let target = fd_clone(fd).ok_or(pipe::Error::BadEnd)?;
    match &target {
        Fd::Event { event } => event.write(value),
        _ => Err(pipe::Error::BadEnd),
    }
}

/// Whether a socket descriptor preserves message boundaries.
pub fn fd_seqpacket(fd: usize) -> bool {
    let tasks = TASKS.lock();
    let Some(task) = tasks[current()].as_ref() else {
        return false;
    };
    if fd >= FD_COUNT {
        return false;
    }
    matches!(&task.fds[fd], Fd::Socket { pair, .. } if pair.seqpacket())
}

/// Most bytes one [`fd_peek`] hands back; a short read is legal, so a huge
/// request is served in pieces instead of duplicating the whole file.
const FD_READ_MAX: usize = 1 << 20;

/// The next up-to-`count` bytes of a file descriptor, *without* advancing its
/// offset. The bytes come back in a kernel buffer so the caller can copy them
/// to user memory through the validated path and only then [`fd_advance`].
///
/// This used to take a raw destination pointer and `copy_nonoverlapping` into it
/// while holding the task-table lock: a user-chosen kernel address was an
/// arbitrary kernel write with file-controlled contents.
pub fn fd_peek(fd: usize, count: usize) -> Option<Vec<u8>> {
    let tasks = TASKS.lock();
    let task = tasks[current()].as_ref()?;
    if fd >= FD_COUNT {
        return None;
    }
    if let Fd::File { data, offset } = &task.fds[fd] {
        let remaining = data.len().saturating_sub(*offset);
        let n = remaining.min(count).min(FD_READ_MAX);
        Some(data[*offset..*offset + n].to_vec())
    } else {
        None
    }
}

/// Advance a file descriptor's offset by `n` bytes after a successful
/// [`fd_peek`] and copy-out.
pub fn fd_advance(fd: usize, n: usize) {
    let mut tasks = TASKS.lock();
    if let Some(task) = tasks[current()].as_mut() {
        if fd < FD_COUNT {
            if let Fd::File { data, offset } = &mut task.fds[fd] {
                *offset = (*offset + n).min(data.len());
            }
        }
    }
}

/// Read up to `count` bytes from a file descriptor and advance its offset.
#[cfg_attr(not(lazyos_tests), allow(dead_code))] // the tests read through it
pub fn fd_read(fd: usize, count: usize) -> Option<Vec<u8>> {
    let bytes = fd_peek(fd, count)?;
    fd_advance(fd, bytes.len());
    Some(bytes)
}

/// File size for a file descriptor (none for terminals/closed).
pub fn fd_size(fd: usize) -> Option<u64> {
    let tasks = TASKS.lock();
    match tasks[current()].as_ref() {
        Some(task) if fd < FD_COUNT => match &task.fds[fd] {
            Fd::File { data, .. } => Some(data.len() as u64),
            _ => None,
        },
        _ => None,
    }
}

/// The current read/write position of a file descriptor.
pub fn fd_offset(fd: usize) -> Option<usize> {
    let tasks = TASKS.lock();
    match tasks[current()].as_ref() {
        Some(task) if fd < FD_COUNT => match &task.fds[fd] {
            Fd::File { offset, .. } => Some(*offset),
            _ => None,
        },
        _ => None,
    }
}

/// Patch `data` into a file descriptor's snapshot at `offset`, extending (and
/// zero-filling) as needed, and advance the descriptor past the write. Returns
/// false unless the descriptor holds a regular file; the Linux ABI uses this
/// to make a writable fd read back its own writes after the backing file was
/// updated.
pub fn fd_apply_write(fd: usize, offset: usize, data: &[u8]) -> bool {
    let mut tasks = TASKS.lock();
    let Some(task) = tasks[current()].as_mut() else {
        return false;
    };
    if fd >= FD_COUNT {
        return false;
    }
    let Fd::File {
        data: buf,
        offset: pos,
    } = &mut task.fds[fd]
    else {
        return false;
    };
    let end = offset.saturating_add(data.len());
    if end > buf.len() {
        buf.resize(end, 0);
    }
    buf[offset..end].copy_from_slice(data);
    *pos = end;
    true
}

/// Reposition a file descriptor (`whence`: 0=SET, 1=CUR, 2=END).
pub fn fd_seek(fd: usize, offset: i64, whence: u64) -> Option<u64> {
    let mut tasks = TASKS.lock();
    let task = tasks[current()].as_mut()?;
    if fd >= FD_COUNT {
        return None;
    }
    if let Fd::File { data, offset: pos } = &mut task.fds[fd] {
        let base = match whence {
            0 => 0i64,
            1 => *pos as i64,
            2 => data.len() as i64,
            _ => return None,
        };
        let new = (base + offset).max(0) as usize;
        *pos = new.min(data.len());
        Some(*pos as u64)
    } else {
        None
    }
}

/// Duplicate a descriptor into the lowest free slot at or above `min`.
/// `dup` uses `min = 3`; `F_DUPFD` passes the caller's argument.
pub fn fd_dup_min(fd: usize, min: usize) -> Option<usize> {
    let mut tasks = TASKS.lock();
    let task = tasks[current()].as_mut()?;
    if fd >= FD_COUNT {
        return None;
    }
    let entry = match &task.fds[fd] {
        Fd::Closed => return None,
        other => other.clone(),
    };
    for index in min.max(3)..FD_COUNT {
        if matches!(task.fds[index], Fd::Closed) {
            task.fds[index] = entry;
            // `dup`/`F_DUPFD` produce a descriptor without `FD_CLOEXEC`.
            task.fd_flags[index] = 0;
            return Some(index);
        }
    }
    None
}

/// Duplicate a descriptor into the lowest free slot (`dup(2)`).
pub fn fd_dup(fd: usize) -> Option<usize> {
    fd_dup_min(fd, 3)
}

/// Duplicate `old` into the specific descriptor `new` (closing it first).
/// `FD_CLOEXEC` is cleared on the new descriptor, as POSIX requires; an
/// `old == new` call is a no-op.
pub fn fd_dup2(old: usize, new: usize) -> Option<usize> {
    if old >= FD_COUNT || new >= FD_COUNT {
        return None;
    }
    if old == new {
        // Validating only: a closed `old` fails, an open one is unchanged.
        return match fd_kind(old) {
            FdKind::Closed => None,
            _ => Some(new),
        };
    }
    let replaced = {
        let mut tasks = TASKS.lock();
        let task = tasks[current()].as_mut()?;
        let entry = match &task.fds[old] {
            Fd::Closed => return None,
            other => other.clone(),
        };
        task.fd_flags[new] = 0;
        core::mem::replace(&mut task.fds[new], entry)
    };
    // The replaced descriptor may have been a pipe end; drop it unlocked.
    drop(replaced);
    Some(new)
}

/// Whether `index` holds a task that is alive (occupied and not finished).
///
/// The display grant uses this to tell a bound compositor apart from a dead
/// one, so the kernel mux can take the screen back without a teardown hook
/// (issue #113). Cheap: one table lock and no allocation.
pub fn live(index: usize) -> bool {
    TASKS.lock().get(index).is_some_and(|task| {
        task.as_ref()
            .is_some_and(|task| task.state != TaskState::Done)
    })
}

/// Snapshot of a task's name, output and done flag, for rendering.
pub fn snapshot(index: usize) -> Option<(&'static str, Vec<u8>, bool)> {
    let tasks = TASKS.lock();
    tasks[index].as_ref().map(|task| {
        (
            task.name,
            task.output.clone(),
            task.state == TaskState::Done,
        )
    })
}

/// One row of [`stats_snapshot`] (issue #144): the task-table view the
/// system-stats syscall copies into its fixed ABI layout. It carries no
/// addresses or credentials, so it is safe to hand to any task.
#[derive(Clone, Copy)]
pub struct StatsRow {
    /// Whether the slot is occupied (a `Done` zombie still counts).
    pub present: bool,
    /// Pid (the slot, see [`process`]).
    pub pid: usize,
    /// Parent pid; `0` is the kernel/init task.
    pub ppid: usize,
    /// Scheduler-visible state.
    pub state: TaskState,
    /// Scheduling class.
    pub class: PriorityClass,
    /// Weight inside the class.
    pub weight: u16,
    /// CPU ticks (100 Hz) charged to this task.
    pub cpu_ticks: u64,
    /// Task name (already interned to `'static`).
    pub name: &'static str,
}

impl StatsRow {
    /// Placeholder for an empty slot; `present` is false.
    const EMPTY: StatsRow = StatsRow {
        present: false,
        pid: 0,
        ppid: 0,
        state: TaskState::Done,
        class: PriorityClass::Normal,
        weight: 0,
        cpu_ticks: 0,
        name: "",
    };
}

/// A whole-table task snapshot for the system-stats syscall (issue #144).
pub struct TaskStats {
    /// One row per scheduler slot (empty slots are `present == false`).
    pub rows: [StatsRow; MAX_TASKS],
    /// Occupied slots whose state is not `Done`.
    pub live: usize,
}

/// Snapshot the task table (one row per slot, no allocation). Takes only the
/// task-table lock, so it can never nest inside another subsystem's lock.
pub fn stats_snapshot() -> TaskStats {
    let tasks = TASKS.lock();
    let mut snapshot = TaskStats {
        rows: [StatsRow::EMPTY; MAX_TASKS],
        live: 0,
    };
    for (slot, task) in tasks.iter().enumerate() {
        let Some(task) = task else { continue };
        snapshot.rows[slot] = StatsRow {
            present: true,
            pid: slot,
            ppid: task.parent,
            state: task.state,
            class: task.class,
            weight: task.weight,
            cpu_ticks: task.cpu_ticks,
            name: task.name,
        };
        if task.state != TaskState::Done {
            snapshot.live += 1;
        }
    }
    snapshot
}

/// Test-harness hooks (issue #62), compiled only with `LAZYOS_TESTS=1`. They let
/// the in-kernel suite drive task bookkeeping without a running scheduler.
#[cfg(lazyos_tests)]
pub mod harness {
    use super::{select_next, PriorityClass, TaskState, WakeReason, KERNEL_TASK, TASKS};

    /// Free every slot except the kernel task's and zero its scheduler
    /// accounting, so tests do not inherit virtual-time or CPU ticks from an
    /// earlier test.
    pub fn reset() {
        let removed: alloc::vec::Vec<super::Task> = {
            let mut tasks = TASKS.lock();
            let removed = tasks
                .iter_mut()
                .skip(1)
                .filter_map(|slot| slot.take())
                .collect();
            if let Some(task) = tasks[KERNEL_TASK].as_mut() {
                task.pass = 0;
                task.cpu_ticks = 0;
                task.class = PriorityClass::Interactive;
                task.weight = PriorityClass::Interactive.default_weight();
            }
            removed
        };
        // Dropping removed tasks closes their pipe ends, which may notify a
        // wait queue; do it with `TASKS` unlocked (queue-before-table order).
        drop(removed);
    }

    /// Mark `index` finished, as if it had called `exit` (re-parenting its
    /// children, like the real path).
    pub fn finish(index: usize, code: u64) {
        super::process::finish(index, code);
    }

    /// Point `current()` at `slot` without a context switch. The tests build
    /// multi-level process trees with `spawn_fork`, which forks the current
    /// task.
    pub fn switch_current(slot: usize) {
        super::CURRENT.store(slot, core::sync::atomic::Ordering::Relaxed);
    }

    /// The state of task `index`.
    pub fn state(index: usize) -> Option<TaskState> {
        TASKS.lock()[index].as_ref().map(|task| task.state)
    }

    /// Classify `fd` in another task's descriptor table, so a test can verify
    /// `fork` inheritance without switching `current()`.
    pub fn fd_kind_at(slot: usize, fd: usize) -> super::FdKind {
        let tasks = TASKS.lock();
        match tasks[slot].as_ref() {
            Some(task) if fd < super::FD_COUNT => match task.fds[fd] {
                super::Fd::Closed => super::FdKind::Closed,
                super::Fd::Terminal => super::FdKind::Terminal,
                super::Fd::File { .. } => super::FdKind::File,
                super::Fd::Pipe { .. } => super::FdKind::Pipe,
                super::Fd::Socket { .. } => super::FdKind::Socket,
                super::Fd::Event { .. } => super::FdKind::EventFd,
                super::Fd::Epoll { .. } => super::FdKind::Epoll,
                super::Fd::UnixListener { .. } => super::FdKind::Listener,
                super::Fd::Unbound { .. } => super::FdKind::Unbound,
            },
            _ => super::FdKind::Closed,
        }
    }

    /// Whether `fd` in another task's table has `FD_CLOEXEC`.
    pub fn fd_cloexec_at(slot: usize, fd: usize) -> bool {
        let tasks = TASKS.lock();
        match tasks[slot].as_ref() {
            Some(task) if fd < super::FD_COUNT => {
                !matches!(task.fds[fd], super::Fd::Closed)
                    && task.fd_flags[fd] & super::FD_CLOEXEC != 0
            }
            _ => false,
        }
    }

    /// The slot the scheduler would pick next, without switching to it or
    /// advancing any pass (a pure query, so it is deterministic).
    pub fn next_runnable() -> usize {
        let tasks = TASKS.lock();
        super::pick_next(&tasks, super::current())
    }

    /// Run one scheduling decision exactly as a timer tick would, without a
    /// context switch: charge the current task a CPU tick, run the stride
    /// selection, point `current()` at the winner, and return it. Tests use
    /// this to simulate N ticks in kernel time (issue #58).
    pub fn simulate_tick() -> usize {
        let mut tasks = TASKS.lock();
        let cur = super::current();
        if let Some(task) = tasks[cur].as_mut() {
            task.cpu_ticks = task.cpu_ticks.saturating_add(1);
        }
        // Same flagging the real tick does (issue #133): finished parentless
        // tasks are handed to `reclaim_pending`, the current one only when the
        // tick actually switches away from it.
        for slot in 1..super::MAX_TASKS {
            if slot != cur {
                super::mark_finished(&tasks, slot);
            }
        }
        let next = select_next(&mut tasks, cur);
        if next != cur {
            super::mark_finished(&tasks, cur);
        }
        super::CURRENT.store(next, core::sync::atomic::Ordering::Relaxed);
        next
    }

    /// The PML4 physical address of task `index`.
    pub fn pml4(index: usize) -> Option<u64> {
        TASKS.lock()[index].as_ref().map(|task| task.pml4)
    }

    /// Run the deadline sweep with an explicit `now`, as a timer tick would.
    pub fn expire_deadlines(now: u64) {
        let mut tasks = TASKS.lock();
        super::expire_deadlines(&mut tasks, now);
    }

    /// Consume the recorded wake reason, as a wait loop does on resume.
    pub fn take_wake_reason(index: usize) -> Option<WakeReason> {
        super::take_wake_reason(index)
    }
}
