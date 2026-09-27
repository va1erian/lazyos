//! POSIX-ish signals: state, delivery, and the kernel-side frame mechanics
//! (issue #60).
//!
//! The model is deliberately small but Linux-shaped:
//!
//! * state is **per process**, keyed by the address space (`pml4`). Tasks that
//!   share a PML4 are threads of one process, so a `kill(pid)` reaches all of
//!   them and one pending set serves the group. This is the documented
//!   simplification: real Linux keeps the blocked mask per thread.
//! * each signal has a [`Disposition`]: default kernel action, ignore, or a
//!   user handler. [`default_action`] classifies the standard dispositions
//!   (term/stop/cont/ignore/core), and `SIGKILL`/`SIGSTOP` are never catchable.
//! * `send` queues a pending bit and wakes a blocked thread with
//!   [`WakeReason::Interrupted`] so the blocking syscall returns `EINTR` at a
//!   point where the signal can be delivered. Fatal signals (`SIGKILL`, and
//!   defaults of term/core signals) end the whole thread group synchronously.
//! * `SIGCHLD` is queued even though its default is ignore: the pending bit is
//!   the observable record of the child event, and parents that installed a
//!   handler are woken for it.
//!
//! Delivery happens at the two boundaries back to ring 3:
//!
//! * `deliver_linux` runs at the end of `process::linux::linux_dispatch`, on
//!   the way out of every Linux syscall. It rebuilds the signal frame from the
//!   context captured at `syscall` entry (plus the syscall result, which is the
//!   value `rt_sigreturn` must restore when the handler returns).
//! * [`sweep`] runs from the timer scheduler for tasks whose saved frame is a
//!   user frame, covering native `int 0x80` programs (whose syscall stub is not
//!   part of this issue's scope) and Linux tasks preempted in user mode. It
//!   applies default actions and delivers handler frames in place.
//!
//! Linux frames follow `struct rt_sigframe` exactly (restorer pointer,
//! `ucontext_t`, `siginfo_t`); `rt_sigreturn` parses the same layout back.

use alloc::vec::Vec;
use spin::Mutex;

use super::{
    current, process, wake_task_with, Kind, Task, TaskState, WaitKind, WakeReason, KERNEL_TASK,
    MAX_TASKS, NEEDS_REDRAW, TASKS,
};

// Signal numbers (x86_64 Linux). The table is complete on purpose: the
// dispatcher must classify any number a Linux binary sends, even if no LazyOS
// code names that signal yet.
#[allow(dead_code)]
pub const SIGHUP: u8 = 1;
pub const SIGINT: u8 = 2;
pub const SIGQUIT: u8 = 3;
pub const SIGILL: u8 = 4;
pub const SIGTRAP: u8 = 5;
pub const SIGABRT: u8 = 6;
pub const SIGBUS: u8 = 7;
pub const SIGFPE: u8 = 8;
pub const SIGKILL: u8 = 9;
#[allow(dead_code)]
pub const SIGUSR1: u8 = 10;
pub const SIGSEGV: u8 = 11;
#[allow(dead_code)]
pub const SIGUSR2: u8 = 12;
#[allow(dead_code)]
pub const SIGPIPE: u8 = 13;
#[allow(dead_code)]
pub const SIGALRM: u8 = 14;
#[allow(dead_code)]
pub const SIGTERM: u8 = 15;
#[allow(dead_code)]
pub const SIGSTKFLT: u8 = 16;
pub const SIGCHLD: u8 = 17;
pub const SIGCONT: u8 = 18;
pub const SIGSTOP: u8 = 19;
pub const SIGTSTP: u8 = 20;
pub const SIGTTIN: u8 = 21;
pub const SIGTTOU: u8 = 22;
pub const SIGURG: u8 = 23;
pub const SIGXCPU: u8 = 24;
pub const SIGXFSZ: u8 = 25;
#[allow(dead_code)]
pub const SIGVTALRM: u8 = 26;
#[allow(dead_code)]
pub const SIGPROF: u8 = 27;
pub const SIGWINCH: u8 = 28;
#[allow(dead_code)]
pub const SIGIO: u8 = 29;
#[allow(dead_code)]
pub const SIGPWR: u8 = 30;
pub const SIGSYS: u8 = 31;

/// Highest supported signal number plus one (`_NSIG`).
pub const NSIG: usize = 65;

// `rt_sigaction` handler sentinels and flags. `SA_RESTORER`/`SA_RESTART` are
// accepted from user space and currently informational.
pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;
pub const SA_SIGINFO: u64 = 0x0000_0004;
#[allow(dead_code)]
pub const SA_RESTORER: u64 = 0x0400_0000;
pub const SA_ONSTACK: u64 = 0x0800_0000;
#[allow(dead_code)]
pub const SA_RESTART: u64 = 0x1000_0000;
pub const SA_NODEFER: u64 = 0x4000_0000;
pub const SA_RESETHAND: u64 = 0x8000_0000;

// `rt_sigprocmask` selectors.
pub const SIG_BLOCK: u64 = 0;
pub const SIG_UNBLOCK: u64 = 1;
pub const SIG_SETMASK: u64 = 2;

// `sigaltstack` flags.
pub const SS_ONSTACK: u32 = 1;
pub const SS_DISABLE: u32 = 2;
/// Minimum alternate stack size Linux accepts (`MINSIGSTKSZ` on x86_64).
pub const MINSIGSTKSZ: u64 = 2048;

// `siginfo.si_code` values used by the kernel.
pub const SI_USER: i32 = 0;
pub const SI_TKILL: i32 = -6;
pub const SI_KERNEL: i32 = 0x80;
pub const SEGV_MAPERR: i32 = 1;
pub const SEGV_ACCERR: i32 = 2;

/// The standard action a signal takes with the default disposition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultAction {
    /// End the process (exit status 128 + signo).
    Term,
    /// End the process; the core-dump flavour is informational here.
    Core,
    /// Park the whole process until `SIGCONT`.
    Stop,
    /// Resume a stopped process.
    Cont,
    /// Drop the signal.
    Ignore,
}

/// The kernel-defined disposition of a signal, from Linux's tables.
pub fn default_action(sig: u8) -> DefaultAction {
    match sig {
        SIGCHLD | SIGURG | SIGWINCH => DefaultAction::Ignore,
        SIGSTOP | SIGTSTP | SIGTTIN | SIGTTOU => DefaultAction::Stop,
        SIGCONT => DefaultAction::Cont,
        SIGQUIT | SIGILL | SIGTRAP | SIGABRT | SIGBUS | SIGFPE | SIGSEGV | SIGXCPU | SIGXFSZ
        | SIGSYS => DefaultAction::Core,
        _ => DefaultAction::Term,
    }
}

/// What a process has installed for a signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    /// Kernel default (see [`default_action`]).
    Default,
    /// Explicitly dropped (`SIG_IGN`).
    Ignore,
    /// A user handler: address, `sa_flags`, the restorer (musl's `__restore_rt`),
    /// and the `sa_mask` applied while the handler runs.
    Handler {
        handler: u64,
        flags: u64,
        restorer: u64,
        mask: u64,
    },
}

/// The alternate signal stack of a process (`sigaltstack`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AltStack {
    pub sp: u64,
    pub size: u64,
    pub enabled: bool,
}

impl AltStack {
    pub const DISABLED: AltStack = AltStack {
        sp: 0,
        size: 0,
        enabled: false,
    };
}

/// `siginfo_t` fields the kernel fills in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SigInfo {
    pub code: i32,
    pub pid: usize,
    pub uid: u32,
    pub addr: u64,
}

impl SigInfo {
    /// A signal sent by a user process (kill/tkill/tgkill).
    pub const fn user(pid: usize, code: i32) -> Self {
        SigInfo {
            code,
            pid,
            uid: 0,
            addr: 0,
        }
    }

    /// A synchronous fault reported by the kernel.
    pub const fn fault(code: i32, addr: u64) -> Self {
        SigInfo {
            code,
            pid: 0,
            uid: 0,
            addr,
        }
    }

    /// A kernel-generated asynchronous event (child exit, terminal INTR).
    pub const fn kernel() -> Self {
        SigInfo {
            code: SI_KERNEL,
            pid: 0,
            uid: 0,
            addr: 0,
        }
    }
}

/// Failures mapped to errno by `process::linux`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignalError {
    /// No live process with that pid.
    NoSuchProcess,
    /// The target exists but may not be signalled (init, or another session
    /// under a stricter credential model; all tasks are root today).
    NotPermitted,
    /// Bad signal number or bad action.
    Invalid,
}

/// Snapshot of the user registers at a delivery boundary. The field order
/// matches the interrupt frame the kernel stacks, so a frame can be copied in
/// and out without shuffling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UserRegs {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub rip: u64,
    pub rsp: u64,
    pub rflags: u64,
}

/// Stride of one process's signal state, fixed by the array above.
struct Signals {
    pml4: u64,
    pending: u64,
    blocked: u64,
    actions: [Disposition; NSIG],
    infos: [SigInfo; NSIG],
    altstack: AltStack,
    /// True while a handler delivered with `SA_ONSTACK` runs, so a nested
    /// signal reuses the current stack instead of re-entering the alt stack.
    on_altstack: bool,
}

impl Signals {
    fn new(pml4: u64) -> Self {
        Signals {
            pml4,
            pending: 0,
            blocked: 0,
            actions: [Disposition::Default; NSIG],
            infos: [SigInfo::user(0, SI_USER); NSIG],
            altstack: AltStack::DISABLED,
            on_altstack: false,
        }
    }
}

/// Process signal state, keyed by address space. A process's PML4 is created at
/// `spawn`/`fork`, freed at reap/exec, and never changes while live, so the key
/// is stable exactly as long as the process is.
static SIGNALS: Mutex<Vec<Signals>> = Mutex::new(Vec::new());

/// Bit for `sig` in a 64-bit set.
const fn bit(sig: u8) -> u64 {
    1u64 << sig
}

/// All signals that can never be blocked or caught.
const UNCATCHABLE: u64 = bit(SIGKILL) | bit(SIGSTOP);

/// A fixed-capacity slot list. Signal paths run in IRQ and scheduler context,
/// where the heap lock may be held by the preempted task, so they must not
/// allocate; `MAX_TASKS` is small enough to collect into a stack array.
struct SlotList {
    slots: [usize; MAX_TASKS],
    len: usize,
}

impl SlotList {
    fn new() -> Self {
        SlotList {
            slots: [0; MAX_TASKS],
            len: 0,
        }
    }

    fn push(&mut self, slot: usize) {
        self.slots[self.len] = slot;
        self.len += 1;
    }

    fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.slots[..self.len].iter().copied()
    }
}

/// The pending set with the uncatchable bits removed (defensive).
const fn clean_mask(mask: u64) -> u64 {
    mask & !UNCATCHABLE
}

/// Lowest numbered signal set in `mask`, ignoring bit 0 (signal 0 is "no
/// signal").
fn lowest_signal(mask: u64) -> Option<u8> {
    let set = mask & !1;
    if set == 0 {
        None
    } else {
        Some(set.trailing_zeros() as u8)
    }
}

/// Run `f` with the signal state of `pml4`, creating it on first use.
fn with_signals<R>(pml4: u64, f: impl FnOnce(&mut Signals) -> R) -> R {
    let mut all = SIGNALS.lock();
    let index = match all.iter().position(|state| state.pml4 == pml4) {
        Some(index) => index,
        None => {
            all.push(Signals::new(pml4));
            all.len() - 1
        }
    };
    f(&mut all[index])
}

/// Drop a torn-down address space's signal state.
pub fn forget(pml4: u64) {
    SIGNALS.lock().retain(|state| state.pml4 != pml4);
}

/// Inherit signal state across `fork`: dispositions, blocked mask and alt stack
/// copy over; a fresh process starts with nothing pending. Ignored signals stay
/// ignored, exactly like Linux; installed handlers are inherited too.
pub fn fork_inherit(parent: u64, child: u64) {
    let mut all = SIGNALS.lock();
    let inherited = match all.iter().find(|state| state.pml4 == parent) {
        Some(state) => {
            let mut copy = Signals::new(child);
            copy.blocked = state.blocked;
            copy.actions = state.actions;
            copy.altstack = state.altstack;
            copy
        }
        None => Signals::new(child),
    };
    all.retain(|state| state.pml4 != child);
    all.push(inherited);
}

/// The `(pml4, kind)` of a live slot.
fn slot_info(slot: usize) -> Option<(u64, Kind)> {
    let tasks = TASKS.lock();
    tasks
        .get(slot)
        .and_then(|task| task.as_ref())
        .map(|task| (task.pml4, task.kind))
}

/// The `(slot, pml4)` of the current task.
fn current_info() -> Option<(usize, u64)> {
    let slot = current();
    slot_info(slot).map(|(pml4, _)| (slot, pml4))
}

/// Dispositions and masks for the Linux shim (`rt_sigaction`/`procmask`/
/// `sigaltstack`). All of these resolve the slot to its process first, then
/// touch only the signal registry: the lock order is always task table ->
/// signal registry, never the reverse.

pub fn action(slot: usize, sig: u8) -> Disposition {
    let Some((pml4, _)) = slot_info(slot) else {
        return Disposition::Default;
    };
    with_signals(pml4, |state| state.actions[sig as usize])
}

pub fn set_action(slot: usize, sig: u8, disposition: Disposition) -> Result<(), SignalError> {
    if sig as usize >= NSIG {
        return Err(SignalError::Invalid);
    }
    if sig == SIGKILL || sig == SIGSTOP {
        return Err(SignalError::Invalid);
    }
    if let Disposition::Handler {
        handler, restorer, ..
    } = disposition
    {
        if handler != SIG_DFL && handler != SIG_IGN && restorer == 0 {
            // musl always passes a restorer; without one the handler could not
            // return into `rt_sigreturn`, and we do not map a vsyscall page.
            return Err(SignalError::Invalid);
        }
    }
    let Some((pml4, _)) = slot_info(slot) else {
        return Err(SignalError::NoSuchProcess);
    };
    with_signals(pml4, |state| state.actions[sig as usize] = disposition);
    Ok(())
}

pub fn blocked(slot: usize) -> u64 {
    let Some((pml4, _)) = slot_info(slot) else {
        return 0;
    };
    with_signals(pml4, |state| state.blocked)
}

pub fn set_blocked(slot: usize, mask: u64) {
    let Some((pml4, _)) = slot_info(slot) else {
        return;
    };
    with_signals(pml4, |state| state.blocked = clean_mask(mask));
}

pub fn altstack(slot: usize) -> AltStack {
    let Some((pml4, _)) = slot_info(slot) else {
        return AltStack::DISABLED;
    };
    with_signals(pml4, |state| state.altstack)
}

/// Install an alternate stack. Returns [`SignalError::Invalid`] for a stack
/// Linux would reject: `SS_DISABLE` is always fine, otherwise the range must be
/// non-empty and at least [`MINSIGSTKSZ`] bytes.
pub fn set_altstack(slot: usize, stack: AltStack) -> Result<(), SignalError> {
    if stack.enabled && (stack.sp == 0 || stack.size < MINSIGSTKSZ) {
        return Err(SignalError::Invalid);
    }
    let Some((pml4, _)) = slot_info(slot) else {
        return Err(SignalError::NoSuchProcess);
    };
    with_signals(pml4, |state| state.altstack = stack);
    Ok(())
}

#[allow(dead_code)] // introspection used by the in-kernel tests
pub fn pending(slot: usize) -> u64 {
    let Some((pml4, _)) = slot_info(slot) else {
        return 0;
    };
    with_signals(pml4, |state| state.pending)
}

/// `kill(pid, sig)` with Linux's pid encoding: positive is a pid, 0 is the
/// caller's process group, -1 is every process except init, and other negatives
/// are a process group.
pub fn kill(caller: usize, pid: i64, sig: u8, info: SigInfo) -> Result<(), SignalError> {
    if sig as usize >= NSIG {
        return Err(SignalError::Invalid);
    }
    match pid {
        0 => kill_group(caller, process::pgid_of(caller), sig, info),
        -1 => kill_all(caller, sig, info),
        pid if pid < 0 => kill_group(caller, (-pid) as usize, sig, info),
        pid => {
            let target = pid as usize;
            if target == KERNEL_TASK {
                return if sig == 0 {
                    Ok(())
                } else {
                    Err(SignalError::NotPermitted)
                };
            }
            if slot_info(target).is_none() {
                return Err(SignalError::NoSuchProcess);
            }
            send_to_slot(caller, target, sig, info)
        }
    }
}

/// `tkill(tid, sig)` / `tgkill(tgid, tid, sig)` after the shim validated `tgid`.
pub fn send_tid(caller: usize, tid: usize, sig: u8, info: SigInfo) -> Result<(), SignalError> {
    if sig as usize >= NSIG {
        return Err(SignalError::Invalid);
    }
    if tid == KERNEL_TASK || slot_info(tid).is_none() {
        return Err(SignalError::NoSuchProcess);
    }
    send_to_slot(caller, tid, sig, info)
}

/// The thread group id of a slot: Linux reports the leader's pid, and LazyOS
/// pids are slots, so the group is identified by the lowest occupied slot that
/// shares the address space (the leader spawned before any thread).
pub fn tgid_of(slot: usize) -> usize {
    let Some((pml4, _)) = slot_info(slot) else {
        return slot;
    };
    let tasks = TASKS.lock();
    (0..MAX_TASKS)
        .find(|&index| tasks[index].as_ref().is_some_and(|task| task.pml4 == pml4))
        .unwrap_or(slot)
}

/// Queue `sig` for `target` (a thread; process state is shared) and, for
/// actionable signals, wake a blocked thread so the event reaches a syscall
/// boundary. Uncachable signals act immediately.
pub fn send_to_slot(
    caller: usize,
    target: usize,
    sig: u8,
    info: SigInfo,
) -> Result<(), SignalError> {
    if sig == 0 {
        return match slot_info(target) {
            Some(_) => Ok(()),
            None => Err(SignalError::NoSuchProcess),
        };
    }
    if sig as usize >= NSIG {
        return Err(SignalError::Invalid);
    }
    let Some((pml4, _)) = slot_info(target) else {
        return Err(SignalError::NoSuchProcess);
    };
    // A signal to a zombie is dropped, like Linux.
    {
        let tasks = TASKS.lock();
        if tasks[target]
            .as_ref()
            .is_some_and(|task| task.state == TaskState::Done)
        {
            return Ok(());
        }
    }
    let info = SigInfo {
        pid: if info.pid == 0 { caller } else { info.pid },
        ..info
    };

    let disposition = with_signals(pml4, |state| state.actions[sig as usize]);
    match sig {
        SIGKILL => {
            terminate_process(pml4, 128 + sig as u64);
            return Ok(());
        }
        SIGSTOP => {
            stop_process(pml4);
            return Ok(());
        }
        SIGCONT => {
            continue_process(pml4);
            // The resume always happens; a *caught* `SIGCONT` then also runs
            // its handler, which is why the default case stops here.
            if !matches!(disposition, Disposition::Handler { .. }) {
                return Ok(());
            }
        }
        _ => {}
    }

    let ignored = match disposition {
        Disposition::Ignore => true,
        Disposition::Default => default_action(sig) == DefaultAction::Ignore,
        Disposition::Handler { .. } => false,
    };
    // `SIGCHLD` is recorded even under its default-ignore disposition: it is
    // the observable child-exit event, and `wait4` keeps using its own queue.
    if ignored && sig != SIGCHLD {
        return Ok(());
    }

    let blocked_now = with_signals(pml4, |state| {
        state.pending |= bit(sig);
        state.infos[sig as usize] = info;
        state.blocked & bit(sig) != 0
    });
    if blocked_now {
        return Ok(());
    }
    let stop_default =
        disposition == Disposition::Default && default_action(sig) == DefaultAction::Stop;
    if stop_default {
        stop_process(pml4);
        return Ok(());
    }
    // Only a handler (or a term/core default) can run user code, so only those
    // interrupt a blocking syscall. `SIGCHLD` with default ignore stays quiet.
    if !ignored {
        wake_blocked_threads(pml4);
    }
    Ok(())
}

/// `kill(-pgid, sig)`: every task in the group, including the caller's group.
fn kill_group(caller: usize, pgid: usize, sig: u8, info: SigInfo) -> Result<(), SignalError> {
    let mut targets = SlotList::new();
    {
        let tasks = TASKS.lock();
        for slot in 1..MAX_TASKS {
            if tasks[slot]
                .as_ref()
                .is_some_and(|task| task.pgid == pgid && task.state != TaskState::Done)
            {
                targets.push(slot);
            }
        }
    }
    if targets.len == 0 {
        return Err(SignalError::NoSuchProcess);
    }
    let mut result = Ok(());
    for target in targets.iter() {
        if let Err(error) = send_to_slot(caller, target, sig, info) {
            result = Err(error);
        }
    }
    result
}

/// `kill(-1, sig)`: everything but init and the kernel.
fn kill_all(caller: usize, sig: u8, info: SigInfo) -> Result<(), SignalError> {
    let mut targets = SlotList::new();
    {
        let tasks = TASKS.lock();
        for slot in 1..MAX_TASKS {
            if tasks[slot]
                .as_ref()
                .is_some_and(|task| task.state != TaskState::Done)
            {
                targets.push(slot);
            }
        }
    }
    if targets.len == 0 {
        return Err(SignalError::NoSuchProcess);
    }
    for target in targets.iter() {
        let _ = send_to_slot(caller, target, sig, info);
    }
    Ok(())
}

/// Terminate every live task sharing `pml4`, recording the same status for
/// each. This is Linux's thread-group exit: fatal signals take the whole
/// process with them.
pub fn terminate_process(pml4: u64, status: u64) -> usize {
    let mut slots = SlotList::new();
    {
        let tasks = TASKS.lock();
        for slot in 1..MAX_TASKS {
            if tasks[slot]
                .as_ref()
                .is_some_and(|task| task.pml4 == pml4 && task.state != TaskState::Done)
            {
                slots.push(slot);
            }
        }
    }
    let mut killed = 0;
    for slot in slots.iter() {
        if process::finish(slot, status) {
            killed += 1;
        }
    }
    killed
}

/// Park every live task sharing `pml4` as stopped (`WaitKind::Signal`).
fn stop_process(pml4: u64) {
    let mut tasks = TASKS.lock();
    for slot in 1..MAX_TASKS {
        let Some(task) = tasks[slot].as_mut() else {
            continue;
        };
        if task.pml4 == pml4 && task.state != TaskState::Done {
            task.state = TaskState::Blocked {
                wait: WaitKind::Signal,
                deadline: None,
            };
            task.wake_reason = None;
        }
    }
}

/// Resume every task sharing `pml4` that a stop signal parked. Pending stop
/// signals are discarded: a `SIGCONT` cancels them, like Linux.
fn continue_process(pml4: u64) {
    with_signals(pml4, |state| {
        state.pending &=
            !(bit(SIGSTOP) | bit(SIGTSTP) | bit(SIGTTIN) | bit(SIGTTOU) | bit(SIGCONT));
    });
    let mut slots = SlotList::new();
    {
        let tasks = TASKS.lock();
        for slot in 1..MAX_TASKS {
            if tasks[slot].as_ref().is_some_and(|task| {
                task.pml4 == pml4
                    && matches!(
                        task.state,
                        TaskState::Blocked {
                            wait: WaitKind::Signal,
                            ..
                        }
                    )
            }) {
                slots.push(slot);
            }
        }
    }
    for slot in slots.iter() {
        wake_task_with(slot, WakeReason::Woken);
    }
}

/// Wake blocked threads of the process so their syscall (or wait loop) can see
/// the signal. Threads parked as stopped stay parked until `SIGCONT`; every
/// other queue is fine to wake directly because the waiter re-reads its state
/// through `take_wake_reason`.
fn wake_blocked_threads(pml4: u64) {
    let mut slots = SlotList::new();
    {
        let tasks = TASKS.lock();
        for slot in 1..MAX_TASKS {
            if tasks[slot].as_ref().is_some_and(|task| {
                task.pml4 == pml4
                    && matches!(task.state, TaskState::Blocked { wait, .. } if wait != WaitKind::Signal)
            }) {
                slots.push(slot);
            }
        }
    }
    for slot in slots.iter() {
        wake_task_with(slot, WakeReason::Interrupted);
    }
}

/// Queue `SIGCHLD` for a parent and wake it only if it asked for the signal
/// (installed a handler). The pending bit is always recorded so the event is
/// observable, but a default-disposition parent is not interrupted: `wait4` is
/// notified by its own wait queue instead.
pub fn post_sigchld(parent: usize) {
    if parent == 0 || parent == KERNEL_TASK {
        return;
    }
    let Some((pml4, _)) = slot_info(parent) else {
        return;
    };
    let disposition = with_signals(pml4, |state| state.actions[SIGCHLD as usize]);
    let _ = send_to_slot(KERNEL_TASK, parent, SIGCHLD, SigInfo::kernel());
    if matches!(disposition, Disposition::Handler { .. }) {
        wake_blocked_threads(pml4);
    }
}

// ---------------------------------------------------------------------------
// Linux signal frames (`struct rt_sigframe`)
// ---------------------------------------------------------------------------

/// Frame word indices for the timer/syscall interrupt frame, after the 15
/// general registers: RIP, CS, RFLAGS, RSP, SS.
pub const FRAME_RIP_INDEX: usize = 15;
/// A page fault frame carries the CPU error code before RIP.
pub const FAULT_RIP_INDEX: usize = 16;

/// Size of the frame we build on the user stack: `pretcode` (8) + `ucontext_t`
/// (304) + `siginfo_t` (128), rounded up with slack.
const LINUX_FRAME_SIZE: u64 = 512;
/// x86_64 System V red zone, which the interrupted code may be using below RSP.
const RED_ZONE: u64 = 128;

/// Offsets inside the Linux frame. `mcontext` is the kernel `struct sigcontext`
/// musl also uses (`mcontext_t`).
mod lf {
    // ucontext_t starts after `pretcode`.
    pub const UC_FLAGS: u64 = 8;
    pub const UC_LINK: u64 = 16;
    pub const UC_STACK: u64 = 24;
    pub const MCONTEXT: u64 = 48;
    pub const UC_SIGMASK: u64 = 304;
    pub const SIGINFO: u64 = 312;
    // sigcontext fields, relative to MCONTEXT.
    pub const R8: u64 = 0;
    pub const R9: u64 = 8;
    pub const R10: u64 = 16;
    pub const R11: u64 = 24;
    pub const R12: u64 = 32;
    pub const R13: u64 = 40;
    pub const R14: u64 = 48;
    pub const R15: u64 = 56;
    pub const RDI: u64 = 64;
    pub const RSI: u64 = 72;
    pub const RBP: u64 = 80;
    pub const RBX: u64 = 88;
    pub const RDX: u64 = 96;
    pub const RAX: u64 = 104;
    pub const RCX: u64 = 112;
    pub const RSP: u64 = 120;
    pub const RIP: u64 = 128;
    pub const EFLAGS: u64 = 136;
    pub const CS: u64 = 144;
    pub const GS: u64 = 146;
    pub const FS: u64 = 148;
    pub const SS: u64 = 150;
    pub const ERR: u64 = 152;
    pub const TRAPNO: u64 = 160;
    pub const OLDMASK: u64 = 168;
    pub const CR2: u64 = 176;
    pub const FPSTATE: u64 = 184;
}

/// Result of laying a handler frame on the user stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameResult {
    /// Entry point the task resumes at.
    pub rip: u64,
    /// Stack pointer the handler runs with (points at `pretcode`).
    pub rsp: u64,
    /// Address of the `siginfo_t` passed to an `SA_SIGINFO` handler.
    pub info: u64,
    /// Address of the `ucontext_t` passed to an `SA_SIGINFO` handler.
    pub ucontext: u64,
}

fn write_u64(addr: u64, value: u64) {
    // Safety: the caller works within a mapped user stack.
    unsafe { core::ptr::write_volatile(addr as *mut u64, value) };
}

fn write_u32(addr: u64, value: u32) {
    // Safety: the caller works within a mapped user stack.
    unsafe { core::ptr::write_volatile(addr as *mut u32, value) };
}

fn write_i32(addr: u64, value: i32) {
    // Safety: the caller works within a mapped user stack.
    unsafe { core::ptr::write_volatile(addr as *mut i32, value) };
}

fn write_u16(addr: u64, value: u16) {
    // Safety: the caller works within a mapped user stack.
    unsafe { core::ptr::write_volatile(addr as *mut u16, value) };
}

fn read_u64(addr: u64) -> u64 {
    // Safety: the caller works within a mapped user stack.
    unsafe { core::ptr::read_volatile(addr as *const u64) }
}

/// Line up the frame below `stack_top`, leaving the red zone free.
fn frame_base(stack_top: u64) -> u64 {
    (stack_top - RED_ZONE - LINUX_FRAME_SIZE) & !0xF
}

/// Build the Linux `rt_sigframe` at the top of `stack_top`. Writes the
/// restorer pointer, `ucontext_t` (with the interrupted registers and the
/// pre-handler mask) and `siginfo_t`, and returns the handler's entry context.
/// The action's `sa_mask` composition is done by the caller before it calls
/// this: `saved_mask` is what `rt_sigreturn` will restore.
pub fn build_linux_frame(
    stack_top: u64,
    regs: &UserRegs,
    sig: u8,
    handler: u64,
    flags: u64,
    restorer: u64,
    mask: u64,
    saved_mask: u64,
    info: &SigInfo,
) -> FrameResult {
    let frame = frame_base(stack_top);
    write_u64(frame, restorer);
    // ucontext_t.
    write_u64(frame + lf::UC_FLAGS, 0);
    write_u64(frame + lf::UC_LINK, 0);
    // uc_stack: report the (empty by default) alternate stack slot musl reads.
    write_u64(frame + lf::UC_STACK, 0);
    write_u32(frame + lf::UC_STACK + 8, 0);
    write_u64(frame + lf::UC_STACK + 16, 0);
    let mc = frame + lf::MCONTEXT;
    write_u64(mc + lf::R8, regs.r8);
    write_u64(mc + lf::R9, regs.r9);
    write_u64(mc + lf::R10, regs.r10);
    write_u64(mc + lf::R11, regs.r11);
    write_u64(mc + lf::R12, regs.r12);
    write_u64(mc + lf::R13, regs.r13);
    write_u64(mc + lf::R14, regs.r14);
    write_u64(mc + lf::R15, regs.r15);
    write_u64(mc + lf::RDI, regs.rdi);
    write_u64(mc + lf::RSI, regs.rsi);
    write_u64(mc + lf::RBP, regs.rbp);
    write_u64(mc + lf::RBX, regs.rbx);
    write_u64(mc + lf::RDX, regs.rdx);
    write_u64(mc + lf::RAX, regs.rax);
    write_u64(mc + lf::RCX, regs.rcx);
    write_u64(mc + lf::RSP, regs.rsp);
    write_u64(mc + lf::RIP, regs.rip);
    write_u64(mc + lf::EFLAGS, regs.rflags);
    let selectors = crate::arch::gdt::selectors();
    write_u16(mc + lf::CS, selectors.user_code);
    write_u16(mc + lf::GS, 0);
    write_u16(mc + lf::FS, 0);
    write_u16(mc + lf::SS, selectors.user_data);
    write_u64(mc + lf::ERR, 0);
    write_u64(mc + lf::TRAPNO, 0);
    write_u64(mc + lf::OLDMASK, mask);
    write_u64(mc + lf::CR2, 0);
    write_u64(mc + lf::FPSTATE, 0);
    write_u64(frame + lf::UC_SIGMASK, saved_mask);
    // siginfo_t.
    let si = frame + lf::SIGINFO;
    write_i32(si, sig as i32);
    write_i32(si + 4, 0);
    write_i32(si + 8, info.code);
    write_u32(si + 12, 0);
    if info.code == SEGV_MAPERR || info.code == SEGV_ACCERR {
        write_u64(si + 16, info.addr);
    } else {
        write_u32(si + 16, info.pid as u32);
        write_u32(si + 20, info.uid);
    }

    // SIG_DFL/SIG_IGN cannot be reached here (the shim replaces them with a
    // Default/Ignore disposition), so `handler` is a real user address.
    let _ = (flags, SIG_DFL, SIG_IGN);
    FrameResult {
        rip: handler,
        rsp: frame,
        info: si,
        ucontext: frame + lf::UC_FLAGS,
    }
}

/// Parse a frame at `user_rsp` (the value `rt_sigreturn` was entered with, i.e.
/// just above `pretcode`) back into the interrupted registers and mask.
pub fn parse_linux_frame(user_rsp: u64) -> (UserRegs, u64) {
    let frame = user_rsp.wrapping_sub(8);
    let mc = frame + lf::MCONTEXT;
    let regs = UserRegs {
        r8: read_u64(mc + lf::R8),
        r9: read_u64(mc + lf::R9),
        r10: read_u64(mc + lf::R10),
        r11: read_u64(mc + lf::R11),
        r12: read_u64(mc + lf::R12),
        r13: read_u64(mc + lf::R13),
        r14: read_u64(mc + lf::R14),
        r15: read_u64(mc + lf::R15),
        rdi: read_u64(mc + lf::RDI),
        rsi: read_u64(mc + lf::RSI),
        rbp: read_u64(mc + lf::RBP),
        rbx: read_u64(mc + lf::RBX),
        rdx: read_u64(mc + lf::RDX),
        rax: read_u64(mc + lf::RAX),
        rcx: read_u64(mc + lf::RCX),
        rsp: read_u64(mc + lf::RSP),
        rip: read_u64(mc + lf::RIP),
        rflags: read_u64(mc + lf::EFLAGS),
    };
    (regs, read_u64(frame + lf::UC_SIGMASK))
}

/// Word layout of a native signal frame: return context first, then the
/// interrupted registers, so a future native `sigreturn` can pop them.
const NATIVE_FRAME_WORDS: u64 = 20;
const NATIVE_FRAME_SIZE: u64 = NATIVE_FRAME_WORDS * 8;

/// Minimal native frame: `[old_rip, old_rsp, old_rflags, sig, GP regs...]`.
/// The handler starts with RSP pointing at `old_rip`, so a bare `ret` returns
/// to the interrupted instruction (register state is not restored; native
/// programs have no restorer yet).
pub fn build_native_frame(stack_top: u64, regs: &UserRegs, sig: u8) -> FrameResult {
    let frame = ((stack_top - RED_ZONE - NATIVE_FRAME_SIZE) & !0xF).max(RED_ZONE + 16);
    write_u64(frame, regs.rip);
    write_u64(frame + 8, regs.rsp);
    write_u64(frame + 16, regs.rflags);
    write_u64(frame + 24, sig as u64);
    let gp = [
        regs.r15, regs.r14, regs.r13, regs.r12, regs.r11, regs.r10, regs.r9, regs.r8, regs.rbp,
        regs.rdi, regs.rsi, regs.rdx, regs.rcx, regs.rbx, regs.rax,
    ];
    for (i, value) in gp.iter().enumerate() {
        write_u64(frame + 32 + i as u64 * 8, *value);
    }
    FrameResult {
        rip: 0, // filled by the caller with the handler address
        rsp: frame,
        info: 0,
        ucontext: 0,
    }
}

/// Parse a native frame back, as a native sigreturn would. Used by the
/// in-kernel round-trip test until a native `sigreturn` syscall exists.
#[allow(dead_code)]
pub fn parse_native_frame(frame: u64) -> (UserRegs, u8) {
    let mut regs = UserRegs {
        rip: read_u64(frame),
        rsp: read_u64(frame + 8),
        rflags: read_u64(frame + 16),
        ..UserRegs::default()
    };
    let gp = [
        &mut regs.r15,
        &mut regs.r14,
        &mut regs.r13,
        &mut regs.r12,
        &mut regs.r11,
        &mut regs.r10,
        &mut regs.r9,
        &mut regs.r8,
        &mut regs.rbp,
        &mut regs.rdi,
        &mut regs.rsi,
        &mut regs.rdx,
        &mut regs.rcx,
        &mut regs.rbx,
        &mut regs.rax,
    ];
    for (i, slot) in gp.into_iter().enumerate() {
        *slot = read_u64(frame + 32 + i as u64 * 8);
    }
    (regs, read_u64(frame + 24) as u8)
}

// ---------------------------------------------------------------------------
// Delivery
// ---------------------------------------------------------------------------

/// The disposition changes made when a handler is entered: the mask in effect
/// while it runs, the saved mask `rt_sigreturn` restores, and the stack choice.
struct Armed {
    handler: u64,
    flags: u64,
    restorer: u64,
    mask: u64,
    saved_mask: u64,
    altstack: AltStack,
    use_altstack: bool,
    info: SigInfo,
}

fn next_deliverable(pml4: u64) -> Option<(u8, Disposition)> {
    with_signals(pml4, |state| {
        let ready = state.pending & !state.blocked;
        lowest_signal(ready).map(|sig| (sig, state.actions[sig as usize]))
    })
}

fn clear_pending(pml4: u64, sig: u8) {
    with_signals(pml4, |state| state.pending &= !bit(sig));
}

/// Consume `sig` into a handler: clear it from pending, apply the action mask
/// (plus the signal itself unless `SA_NODEFER`), arm the alternate stack, and
/// honour `SA_RESETHAND`.
fn arm_handler(pml4: u64, sig: u8) -> Option<Armed> {
    with_signals(pml4, |state| {
        let Disposition::Handler {
            handler,
            flags,
            restorer,
            mask,
        } = state.actions[sig as usize]
        else {
            return None;
        };
        let saved_mask = state.blocked;
        let mut next = saved_mask | mask;
        if flags & SA_NODEFER == 0 {
            next |= bit(sig);
        }
        state.blocked = clean_mask(next);
        state.pending &= !bit(sig);
        let use_altstack = flags & SA_ONSTACK != 0 && state.altstack.enabled && !state.on_altstack;
        if use_altstack {
            state.on_altstack = true;
        }
        if flags & SA_RESETHAND != 0 {
            state.actions[sig as usize] = Disposition::Default;
        }
        let info = state.infos[sig as usize];
        Some(Armed {
            handler,
            flags,
            restorer,
            mask,
            saved_mask,
            altstack: state.altstack,
            use_altstack,
            info,
        })
    })
}

/// Where a handler frame goes: the alternate stack when requested, otherwise
/// the interrupted stack.
fn handler_stack_top(regs: &UserRegs, armed: &Armed) -> u64 {
    if armed.use_altstack {
        armed.altstack.sp + armed.altstack.size
    } else {
        regs.rsp
    }
}

/// Write a delivered signal's frame and return the handler entry context.
fn prepare_handler(regs: &UserRegs, sig: u8, armed: &Armed, native: bool) -> FrameResult {
    let stack_top = handler_stack_top(regs, armed);
    if native {
        let mut result = build_native_frame(stack_top, regs, sig);
        result.rip = armed.handler;
        result
    } else {
        build_linux_frame(
            stack_top,
            regs,
            sig,
            armed.handler,
            armed.flags,
            armed.restorer,
            armed.mask,
            armed.saved_mask,
            &armed.info,
        )
    }
}

/// Copy a frame onto the syscall return path: `sysretq` will resume at
/// `regs.rip` with the saved general registers reloaded.
fn apply_linux_frame_syscall(regs: &UserRegs) {
    crate::arch::linux::set_user_return(regs.rip, regs.rsp, regs.rflags);
    let saved = [
        (0usize, regs.r15),
        (1, regs.r14),
        (2, regs.r13),
        (3, regs.r12),
        (4, regs.rbp),
        (5, regs.rbx),
        (6, regs.rdi),
        (7, regs.rsi),
        (8, regs.rdx),
        (9, regs.r8),
        (10, regs.r9),
        (11, regs.r10),
    ];
    for (slot, value) in saved {
        crate::arch::linux::set_saved_register(slot, value);
    }
    // Keep the captured context in step so a nested delivery (or `clone`) sees
    // the handler's entry state rather than the syscall's.
    let context = crate::arch::linux::UserContext {
        rip: regs.rip,
        rflags: regs.rflags,
        rsp: regs.rsp,
        rbx: regs.rbx,
        rbp: regs.rbp,
        r12: regs.r12,
        r13: regs.r13,
        r14: regs.r14,
        r15: regs.r15,
        rdi: regs.rdi,
        rsi: regs.rsi,
        rdx: regs.rdx,
        r8: regs.r8,
        r9: regs.r9,
        r10: regs.r10,
    };
    crate::arch::linux::set_user_context(context);
}

/// Rebuild the interrupted user context from this task's syscall-return stack.
///
/// The words pushed by `linux_syscall_entry` live on the *task's own* kernel
/// stack, so unlike the global `USER_CONTEXT` snapshot they cannot be
/// clobbered by another task that runs while this one is blocked inside its
/// syscall. `rax` is passed in (the syscall result the return path holds);
/// `r11`/`rcx` mirror the saved flags/rip, exactly as the `syscall` instruction
/// left them.
fn saved_regs_from_stack(rax: u64) -> UserRegs {
    // Safety: we are inside the current task's syscall; the entry stub pushed
    // these 15 words at fixed offsets below the kernel stack top.
    let word = |slot: isize| unsafe {
        core::ptr::read_volatile((crate::arch::linux::KERNEL_STACK as *const u64).offset(slot))
    };
    let rsp = word(-1);
    let rflags = word(-2);
    let rip = word(-3);
    UserRegs {
        r15: word(-4),
        r14: word(-5),
        r13: word(-6),
        r12: word(-7),
        rbp: word(-8),
        rbx: word(-9),
        rdi: word(-10),
        rsi: word(-11),
        rdx: word(-12),
        r8: word(-13),
        r9: word(-14),
        r10: word(-15),
        r11: rflags,
        rcx: rip,
        rax,
        rip,
        rsp,
        rflags,
    }
}

/// Halt the CPU until the scheduler runs another task.
fn halt_forever() -> ! {
    loop {
        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
    }
}

/// Park the caller while its process is stopped, resuming on `SIGCONT`. Used
/// when a delivery boundary meets a stop default. A `SIGKILL` while stopped
/// marks the task `Done`; there is nothing to resume, so it halts like any
/// other termination (the scheduler has already moved on).
fn wait_continued() {
    loop {
        let state = {
            let tasks = TASKS.lock();
            tasks[current()].as_ref().map(|task| task.state)
        };
        match state {
            Some(TaskState::Blocked {
                wait: WaitKind::Signal,
                ..
            }) => x86_64::instructions::interrupts::enable_and_hlt(),
            Some(TaskState::Done) | None => halt_forever(),
            _ => return,
        }
    }
}

/// Apply one disposition to the current task, returning the (possibly updated)
/// register context for any further pending signal. Stops park here until
/// `SIGCONT`; fatal defaults never return.
fn apply_action(pml4: u64, sig: u8, disposition: Disposition, regs: &mut UserRegs) -> bool {
    let action = match disposition {
        Disposition::Ignore => {
            clear_pending(pml4, sig);
            return true;
        }
        Disposition::Default => default_action(sig),
        Disposition::Handler { .. } => {
            let Some(armed) = arm_handler(pml4, sig) else {
                return false;
            };
            let result = prepare_handler(regs, sig, &armed, false);
            regs.rip = result.rip;
            regs.rsp = result.rsp;
            regs.rdi = sig as u64;
            if armed.flags & SA_SIGINFO != 0 {
                regs.rsi = result.info;
                regs.rdx = result.ucontext;
            }
            return true;
        }
    };
    match action {
        DefaultAction::Ignore => clear_pending(pml4, sig),
        DefaultAction::Cont => clear_pending(pml4, sig),
        DefaultAction::Term | DefaultAction::Core => {
            if default_action(sig) == DefaultAction::Core {
                serial_println!("signal: task {} core-dumped on signal {sig}", current());
            }
            terminate_process(pml4, 128 + sig as u64);
            halt_forever();
        }
        DefaultAction::Stop => {
            stop_process(pml4);
            wait_continued();
        }
    }
    true
}

/// Deliver pending signals on the way out of a Linux syscall. `result` is the
/// value `sysretq` would return; it is recorded as `rax` in the frame so
/// `rt_sigreturn` resumes the caller with the syscall's outcome (typically
/// `-EINTR`).
pub fn deliver_linux(result: u64) {
    let Some((slot, pml4)) = current_info() else {
        return;
    };
    let is_linux = {
        let tasks = TASKS.lock();
        tasks[slot]
            .as_ref()
            .is_some_and(|task| task.kind == Kind::Linux && task.kstack_top != 0)
    };
    if !is_linux {
        return;
    }
    let mut regs = saved_regs_from_stack(result);
    let mut frame_written = false;
    loop {
        let Some((sig, disposition)) = next_deliverable(pml4) else {
            break;
        };
        if default_action(sig) == DefaultAction::Stop && disposition == Disposition::Default {
            stop_process(pml4);
            wait_continued();
            continue;
        }
        apply_action(pml4, sig, disposition, &mut regs);
        frame_written = true;
    }
    if frame_written {
        apply_linux_frame_syscall(&regs);
    }
}

/// Deliver `SIGSEGV` for a page fault that COW/demand-zero could not resolve.
/// Returns true when a handler was entered: the caller resumes the faulting
/// task at the handler instead of halting the machine. Without a handler
/// (ignore, and every default flavour) the caller keeps today's diagnostic
/// halt.
pub fn deliver_fault(frame_rsp: u64, rip_index: usize, fault_addr: u64, error: u64) -> bool {
    let Some((slot, pml4)) = current_info() else {
        return false;
    };
    let disposition = with_signals(pml4, |state| state.actions[SIGSEGV as usize]);
    let Disposition::Handler { .. } = disposition else {
        return false;
    };
    let info = SigInfo::fault(
        if error & 0b10 != 0 {
            SEGV_ACCERR
        } else {
            SEGV_MAPERR
        },
        fault_addr,
    );
    with_signals(pml4, |state| state.infos[SIGSEGV as usize] = info);
    let Some(armed) = arm_handler(pml4, SIGSEGV) else {
        return false;
    };
    let native = {
        let tasks = TASKS.lock();
        tasks[slot]
            .as_ref()
            .is_some_and(|task| task.kind == Kind::Native || task.kstack_top == 0)
    };
    // Safety: the caller passes the base of the exception frame it received.
    let mut regs = unsafe { regs_from_frame(frame_rsp, rip_index) };
    let result = prepare_handler(&regs, SIGSEGV, &armed, native);
    regs.rip = result.rip;
    regs.rsp = result.rsp;
    regs.rdi = SIGSEGV as u64;
    if !native && armed.flags & SA_SIGINFO != 0 {
        regs.rsi = result.info;
        regs.rdx = result.ucontext;
    }
    // Safety: same frame, now rewritten in place.
    unsafe { apply_regs_to_frame(frame_rsp, &regs, rip_index) };
    true
}

/// One task the timer sweep ended, to be finished with its side effects after
/// the task table lock is dropped.
#[derive(Clone, Copy)]
pub struct SweepFinish {
    /// The task that was ended (diagnostics).
    #[allow(dead_code)]
    pub slot: usize,
    /// Its parent, to receive `SIGCHLD` once the table lock is dropped.
    pub parent: usize,
    /// The exit status recorded (diagnostics).
    #[allow(dead_code)]
    pub status: u64,
}

const NO_FINISH: SweepFinish = SweepFinish {
    slot: 0,
    parent: 0,
    status: 0,
};

/// Apply default actions and handler frames to every runnable task whose saved
/// frame is a user frame. Runs on the scheduler's lock, so it mutates the task
/// table in place and returns the terminations (with their length) for
/// post-processing after the lock is dropped.
///
/// # Safety
/// `tasks` must be the live task table; each `Task::rsp` must point at an
/// interrupt frame (the scheduler stores exactly that).
pub unsafe fn sweep(tasks: &mut [Option<Task>; MAX_TASKS]) -> ([SweepFinish; MAX_TASKS], usize) {
    let mut finished = [NO_FINISH; MAX_TASKS];
    let mut finished_len = 0;
    for slot in 1..MAX_TASKS {
        let (pml4, kind, rsp) = {
            let Some(task) = tasks[slot].as_ref() else {
                continue;
            };
            if task.state != TaskState::Runnable {
                continue;
            }
            (task.pml4, task.kind, task.rsp)
        };
        // Only a frame saved from ring 3 can take a user handler; a task parked
        // inside the kernel (woken wait) is delivered by its syscall return.
        if !frame_is_user(rsp, FRAME_RIP_INDEX) {
            continue;
        }
        loop {
            let Some((sig, disposition)) = next_deliverable(pml4) else {
                break;
            };
            if disposition == Disposition::Ignore {
                clear_pending(pml4, sig);
                continue;
            }
            if disposition == Disposition::Default {
                match default_action(sig) {
                    DefaultAction::Ignore | DefaultAction::Cont => {
                        clear_pending(pml4, sig);
                        continue;
                    }
                    DefaultAction::Stop => {
                        let task = tasks[slot].as_mut().unwrap();
                        task.state = TaskState::Blocked {
                            wait: WaitKind::Signal,
                            deadline: None,
                        };
                        task.wake_reason = None;
                        break;
                    }
                    DefaultAction::Term | DefaultAction::Core => {
                        let status = 128 + sig as u64;
                        if let Some(parent) = process::finish_locked(tasks, slot, status) {
                            finished[finished_len] = SweepFinish {
                                slot,
                                parent,
                                status,
                            };
                            finished_len += 1;
                        }
                        break;
                    }
                }
            }
            // Handler: rewrite the saved frame in place.
            let Some(armed) = arm_handler(pml4, sig) else {
                break;
            };
            let mut regs = regs_from_frame(rsp, FRAME_RIP_INDEX);
            let result = prepare_handler(&regs, sig, &armed, kind != Kind::Linux);
            regs.rip = result.rip;
            regs.rsp = result.rsp;
            regs.rdi = sig as u64;
            if kind == Kind::Linux && armed.flags & SA_SIGINFO != 0 {
                regs.rsi = result.info;
                regs.rdx = result.ucontext;
            }
            apply_regs_to_frame(rsp, &regs, FRAME_RIP_INDEX);
            break;
        }
    }
    (finished, finished_len)
}

/// Finish a sweep's terminations: repaint, wake `wait4`, and post `SIGCHLD`.
/// Must be called with the task table lock released.
pub fn finish_sweep(finished: &[SweepFinish]) {
    if finished.is_empty() {
        return;
    }
    NEEDS_REDRAW.store(true, core::sync::atomic::Ordering::Relaxed);
    for done in finished {
        if done.parent != KERNEL_TASK && done.parent != 0 {
            post_sigchld(done.parent);
        }
    }
    super::wait::CHILD_EXIT.notify_all();
}

// ---------------------------------------------------------------------------
// Saved user frame access (timer and page-fault layouts)
// ---------------------------------------------------------------------------

fn frame_word(rsp: u64, index: usize) -> u64 {
    // Safety: `rsp` points at an interrupt frame the kernel saved.
    unsafe { core::ptr::read_volatile((rsp + index as u64 * 8) as *const u64) }
}

fn put_frame_word(rsp: u64, index: usize, value: u64) {
    // Safety: `rsp` points at an interrupt frame the kernel saved.
    unsafe { core::ptr::write_volatile((rsp + index as u64 * 8) as *mut u64, value) };
}

/// Read a saved interrupt frame into a register context. `rip_index` is 15 for
/// timer/syscall frames and 16 for a page fault (whose error code sits first).
pub(crate) unsafe fn regs_from_frame(rsp: u64, rip_index: usize) -> UserRegs {
    UserRegs {
        r15: frame_word(rsp, 0),
        r14: frame_word(rsp, 1),
        r13: frame_word(rsp, 2),
        r12: frame_word(rsp, 3),
        r11: frame_word(rsp, 4),
        r10: frame_word(rsp, 5),
        r9: frame_word(rsp, 6),
        r8: frame_word(rsp, 7),
        rbp: frame_word(rsp, 8),
        rdi: frame_word(rsp, 9),
        rsi: frame_word(rsp, 10),
        rdx: frame_word(rsp, 11),
        rcx: frame_word(rsp, 12),
        rbx: frame_word(rsp, 13),
        rax: frame_word(rsp, 14),
        rip: frame_word(rsp, rip_index),
        rsp: frame_word(rsp, rip_index + 3),
        rflags: frame_word(rsp, rip_index + 2),
    }
}

/// Rewrite a saved interrupt frame in place; the inverse of [`regs_from_frame`].
pub(crate) unsafe fn apply_regs_to_frame(rsp: u64, regs: &UserRegs, rip_index: usize) {
    let gp = [
        regs.r15, regs.r14, regs.r13, regs.r12, regs.r11, regs.r10, regs.r9, regs.r8, regs.rbp,
        regs.rdi, regs.rsi, regs.rdx, regs.rcx, regs.rbx, regs.rax,
    ];
    for (i, value) in gp.iter().enumerate() {
        put_frame_word(rsp, i, *value);
    }
    put_frame_word(rsp, rip_index, regs.rip);
    put_frame_word(rsp, rip_index + 2, regs.rflags);
    put_frame_word(rsp, rip_index + 3, regs.rsp);
}

/// Whether a saved frame's `CS` says ring 3.
fn frame_is_user(rsp: u64, rip_index: usize) -> bool {
    frame_word(rsp, rip_index + 1) & 3 == 3
}

/// Test-harness hooks (issue #62): reset the registry between tests.
#[cfg(laZYOS_TESTS)]
pub mod harness {
    /// Clear all process signal state.
    pub fn reset() {
        super::SIGNALS.lock().clear();
    }

    /// Number of process entries (leak check for tests).
    pub fn registry_len() -> usize {
        super::SIGNALS.lock().len()
    }
}
