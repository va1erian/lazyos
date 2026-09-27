//! Preemptive round-robin scheduling and ring-3 tasks.
//!
//! The timer ISR ([`switch::timer_isr`]) pushes the general-purpose registers,
//! calls [`schedule`], and resumes whatever `schedule` returns. Each task has
//! its own address space (PML4), kernel stack, and terminal (output buffer +
//! input queue).

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;
use x86_64::PhysAddr;

use crate::arch::gdt;
use crate::input::keyboard::Key;
use crate::mem;
use crate::process;

pub mod switch;

/// Slots: 0 is the kernel (multiplexer), 1.. are user programs/threads.
pub const MAX_TASKS: usize = 16;
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
/// True once the scheduler is running (changes how `exit` behaves).
static SCHEDULING: AtomicBool = AtomicBool::new(false);

/// Which syscall ABI a task uses.
#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    /// LazyOS native `int 0x80` programs.
    Native,
    /// Linux `syscall`/`sysret` binaries.
    Linux,
}

/// Number of file descriptors per task.
pub const FD_COUNT: usize = 16;

/// A Linux file descriptor slot.
pub enum Fd {
    /// Unused slot.
    Closed,
    /// stdin/stdout/stderr (and `/dev/tty`): the task's own terminal.
    Terminal,
    /// A regular file: contents read at open time plus the current offset.
    File { data: Vec<u8>, offset: usize },
}

/// Cheap classification of a descriptor for syscall dispatch.
#[derive(Clone, Copy, PartialEq)]
pub enum FdKind {
    Closed,
    Terminal,
    File,
}

fn new_fds() -> [Fd; FD_COUNT] {
    // 0/1/2 are the standard streams.
    core::array::from_fn(|i| if i < 3 { Fd::Terminal } else { Fd::Closed })
}

/// Copy a descriptor table (for `fork`; file buffers are duplicated).
fn clone_fds(fds: &[Fd; FD_COUNT]) -> [Fd; FD_COUNT] {
    core::array::from_fn(|i| match &fds[i] {
        Fd::Closed => Fd::Closed,
        Fd::Terminal => Fd::Terminal,
        Fd::File { data, offset } => Fd::File {
            data: data.clone(),
            offset: *offset,
        },
    })
}

pub struct Task {
    pub name: &'static str,
    #[allow(dead_code)] // Kept for per-kind behaviour as the shim grows.
    pub kind: Kind,
    pub pml4: u64,
    pub kstack_top: u64,
    pub rsp: u64,
    pub done: bool,
    /// Parked (e.g. in `futex`) until woken; skipped by the scheduler.
    pub blocked: bool,
    /// Linux `clear_child_tid`: zeroed and futex-woken on thread exit.
    pub clear_child_tid: u64,
    /// Slot of the parent process (0 = none: kernel and threads).
    pub parent: usize,
    /// Exit status, valid once `done`.
    pub exit_status: u64,
    /// Native `sbrk` heap break.
    pub heap_break: u64,
    /// Linux thread pointer (`%fs` base).
    pub fs_base: u64,
    /// Linux file descriptors.
    pub fds: [Fd; FD_COUNT],
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
        done: false,
        blocked: false,
        clear_child_tid: 0,
        parent: 0,
        exit_status: 0,
        heap_break: 0,
        fs_base: 0,
        fds: new_fds(),
        output: Vec::new(),
        input: VecDeque::new(),
    });
}
/// Create a user task from an ELF image. Returns its slot index.
pub fn spawn(name: &'static str, elf: &[u8]) -> Result<usize, &'static str> {
    let mut tasks = TASKS.lock();
    let index = (1..MAX_TASKS)
        .find(|&i| tasks[i].is_none())
        .ok_or("no free task slot")?;

    let pml4 = mem::new_user_table().ok_or("out of memory")?;
    let entry = match process::load_image(pml4, elf) {
        Ok(entry) => entry,
        Err(err) => {
            // A partially loaded image still owns its frames: release them.
            mem::free_user_table(pml4);
            return Err(err);
        }
    };

    let top = kstack_top(index);
    let rsp = build_user_frame(top, entry, process::USER_STACK_TOP - 16);

    tasks[index] = Some(Task {
        name,
        kind: Kind::Native,
        pml4: pml4.as_u64(),
        kstack_top: top,
        rsp,
        done: false,
        blocked: false,
        clear_child_tid: 0,
        parent: 0,
        exit_status: 0,
        heap_break: process::USER_HEAP_BASE,
        fs_base: 0,
        fds: new_fds(),
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
    let (entry, stack_top) = match process::linux::load(pml4, elf, argv0) {
        Ok(loaded) => loaded,
        Err(err) => {
            // A partially loaded image still owns its frames: release them.
            mem::free_user_table(pml4);
            return Err(err);
        }
    };

    let top = kstack_top(index);
    let rsp = build_user_frame(top, entry, stack_top);

    tasks[index] = Some(Task {
        name,
        kind: Kind::Linux,
        pml4: pml4.as_u64(),
        kstack_top: top,
        rsp,
        done: false,
        blocked: false,
        clear_child_tid: 0,
        parent: 0,
        exit_status: 0,
        heap_break: 0,
        fs_base: 0,
        fds: new_fds(),
        output: Vec::new(),
        input: VecDeque::new(),
    });
    register_bumps(
        pml4.as_u64(),
        process::linux::BRK_BASE,
        process::linux::MMAP_BASE,
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
    let context = crate::arch::linux::user_context();

    let top = kstack_top(index);
    let rsp = build_thread_frame(top, &context, user_rsp);

    tasks[index] = Some(Task {
        name,
        kind: Kind::Linux,
        pml4,
        kstack_top: top,
        rsp,
        done: false,
        blocked: false,
        clear_child_tid,
        parent: 0,
        exit_status: 0,
        heap_break: 0,
        fs_base,
        fds: new_fds(),
        output: Vec::new(),
        input: VecDeque::new(),
    });
    Ok(index)
}

/// Fork the current Linux process: a new task with a deep copy of its address
/// space. Returns the child's slot (the parent's `fork` result); the child's
/// frame resumes at the parent's return address with `rax = 0`.
pub fn spawn_fork() -> Result<usize, &'static str> {
    let mut tasks = TASKS.lock();
    let index = (1..MAX_TASKS)
        .find(|&i| tasks[i].is_none())
        .ok_or("no free task slot")?;
    let parent_index = current();
    let parent = tasks[parent_index].as_ref().ok_or("no parent task")?;
    let pml4 = parent.pml4;
    let fs_base = parent.fs_base;
    let (brk, mmap_next) = bump_for_pml4(pml4);
    let context = crate::arch::linux::user_context();
    let fds = clone_fds(&parent.fds);

    let child_table = mem::clone_user_table(PhysAddr::new(pml4)).ok_or("out of memory (fork)")?;
    let top = kstack_top(index);
    let rsp = build_thread_frame(top, &context, context.rsp);

    tasks[index] = Some(Task {
        name: "fork",
        kind: Kind::Linux,
        pml4: child_table.as_u64(),
        kstack_top: top,
        rsp,
        done: false,
        blocked: false,
        clear_child_tid: 0,
        parent: parent_index,
        exit_status: 0,
        heap_break: 0,
        fs_base,
        fds,
        output: Vec::new(),
        input: VecDeque::new(),
    });
    drop(tasks);

    register_bumps(child_table.as_u64(), brk, mmap_next);
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

/// Mark the current task finished with an exit status.
pub fn finish_current(code: u64) {
    let mut tasks = TASKS.lock();
    if let Some(task) = tasks[current()].as_mut() {
        task.done = true;
        task.blocked = false;
        task.exit_status = code;
    }
    drop(tasks);
    NEEDS_REDRAW.store(true, Ordering::Relaxed);
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
    let (index, status, pml4, shared) = {
        let mut tasks = TASKS.lock();
        let mut found = None;
        for index in 1..MAX_TASKS {
            let finished = tasks[index]
                .as_ref()
                .map(|task| task.parent == me && task.done)
                .unwrap_or(false);
            if finished {
                let task = tasks[index].as_ref().unwrap();
                let status = task.exit_status;
                let pml4 = task.pml4;
                let shared = tasks.iter().enumerate().any(|(other, task)| {
                    other != index && task.as_ref().is_some_and(|task| task.pml4 == pml4)
                });
                tasks[index] = None;
                found = Some((index, status, pml4, shared));
                break;
            }
        }
        found?
    };
    if !shared {
        forget_bumps(pml4);
        let pages = mem::user_table_frame_count(PhysAddr::new(pml4));
        let released = mem::free_user_table(PhysAddr::new(pml4));
        let stats = mem::frame_stats();
        serial_println!(
            "mem: reaped task {index}: {pages} pages, released {released} frames, {} free of {}",
            stats.free,
            stats.total
        );
    }
    Some((index, status))
}

/// Park the current task (skipped by the scheduler until woken).
pub fn set_blocked(blocked: bool) {
    if let Some(task) = TASKS.lock()[current()].as_mut() {
        task.blocked = blocked;
    }
}

/// Whether the current task is parked.
pub fn blocked() -> bool {
    TASKS.lock()[current()]
        .as_ref()
        .map(|task| task.blocked)
        .unwrap_or(false)
}

/// Wake a parked task by slot index.
pub fn wake_task(index: usize) {
    if let Some(task) = TASKS.lock()[index].as_mut() {
        task.blocked = false;
    }
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
    }

    // Round-robin to the next runnable task.
    let mut next = cur;
    for step in 1..=MAX_TASKS {
        let candidate = (cur + step) % MAX_TASKS;
        if let Some(task) = tasks[candidate].as_ref() {
            if !task.done && !task.blocked {
                next = candidate;
                break;
            }
        }
    }
    if next == cur {
        return current_rsp;
    }

    CURRENT.store(next, Ordering::Relaxed);
    let task = tasks[next].as_ref().unwrap();
    let (pml4, kstack_top, rsp, fs_base) = (task.pml4, task.kstack_top, task.rsp, task.fs_base);
    drop(tasks);

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
    let focus = FOCUS.load(Ordering::Relaxed);
    let mut tasks = TASKS.lock();
    if let Some(task) = tasks[focus].as_mut() {
        task.input.push_back(key);
    }
}

/// Inject bytes into the current process's input queue (e.g. a terminal reply).
pub fn inject_input(bytes: &[u8]) {
    let mut tasks = TASKS.lock();
    let root = root_index(&tasks);
    if let Some(task) = tasks[root].as_mut() {
        for &byte in bytes {
            task.input.push_back(Key::Char(byte as char));
        }
    }
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

fn cycle_focus() {
    let tasks = TASKS.lock();
    let start = FOCUS.load(Ordering::Relaxed);
    for step in 1..=MAX_TASKS {
        let candidate = (start + step) % MAX_TASKS;
        if candidate == KERNEL_TASK {
            continue;
        }
        if let Some(task) = tasks[candidate].as_ref() {
            if !task.done {
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
            return Some(index);
        }
    }
    None
}

/// Close a descriptor.
pub fn fd_close(fd: usize) -> bool {
    let mut tasks = TASKS.lock();
    match tasks[current()].as_mut() {
        Some(task) if fd < FD_COUNT && !matches!(task.fds[fd], Fd::Closed) => {
            task.fds[fd] = Fd::Closed;
            true
        }
        _ => false,
    }
}

/// Classify a descriptor.
pub fn fd_kind(fd: usize) -> FdKind {
    let tasks = TASKS.lock();
    match tasks[current()].as_ref() {
        Some(task) if fd < FD_COUNT => match task.fds[fd] {
            Fd::Closed => FdKind::Closed,
            Fd::Terminal => FdKind::Terminal,
            Fd::File { .. } => FdKind::File,
        },
        _ => FdKind::Closed,
    }
}

/// Read up to `count` bytes from a file descriptor into `dst`.
pub fn fd_read(fd: usize, dst: *mut u8, count: usize) -> Option<usize> {
    let mut tasks = TASKS.lock();
    let task = tasks[current()].as_mut()?;
    if fd >= FD_COUNT {
        return None;
    }
    if let Fd::File { data, offset } = &mut task.fds[fd] {
        let remaining = data.len().saturating_sub(*offset);
        let n = remaining.min(count);
        // Safety: the caller guarantees `dst` is writable for `n` bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(data[*offset..*offset + n].as_ptr(), dst, n);
        }
        *offset += n;
        Some(n)
    } else {
        None
    }
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

/// Duplicate a descriptor into the lowest free slot.
pub fn fd_dup(fd: usize) -> Option<usize> {
    let mut tasks = TASKS.lock();
    let task = tasks[current()].as_mut()?;
    if fd >= FD_COUNT {
        return None;
    }
    let entry = match &task.fds[fd] {
        Fd::Closed => return None,
        Fd::Terminal => Fd::Terminal,
        Fd::File { data, offset } => Fd::File {
            data: data.clone(),
            offset: *offset,
        },
    };
    for index in 3..FD_COUNT {
        if matches!(task.fds[index], Fd::Closed) {
            task.fds[index] = entry;
            return Some(index);
        }
    }
    None
}

/// Duplicate `old` into the specific descriptor `new` (closing it first).
pub fn fd_dup2(old: usize, new: usize) -> Option<usize> {
    let mut tasks = TASKS.lock();
    let task = tasks[current()].as_mut()?;
    if old >= FD_COUNT || new >= FD_COUNT {
        return None;
    }
    let entry = match &task.fds[old] {
        Fd::Closed => return None,
        Fd::Terminal => Fd::Terminal,
        Fd::File { data, offset } => Fd::File {
            data: data.clone(),
            offset: *offset,
        },
    };
    task.fds[new] = entry;
    Some(new)
}

/// Snapshot of a task's name, output and done flag, for rendering.
pub fn snapshot(index: usize) -> Option<(&'static str, Vec<u8>, bool)> {
    let tasks = TASKS.lock();
    tasks[index]
        .as_ref()
        .map(|task| (task.name, task.output.clone(), task.done))
}
