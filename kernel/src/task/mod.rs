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

/// Slots: 0 is the kernel (multiplexer), 1.. are user programs.
pub const MAX_TASKS: usize = 4;
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

pub struct Task {
    pub name: &'static str,
    #[allow(dead_code)] // Kept for per-kind behaviour as the shim grows.
    pub kind: Kind,
    pub pml4: u64,
    pub kstack_top: u64,
    pub rsp: u64,
    pub done: bool,
    /// Native `sbrk` heap break.
    pub heap_break: u64,
    /// Linux `brk` program break.
    pub brk: u64,
    /// Linux anonymous `mmap` bump pointer.
    pub mmap_next: u64,
    /// Linux thread pointer (`%fs` base).
    pub fs_base: u64,
    /// Linux file descriptors.
    pub fds: [Fd; FD_COUNT],
    pub output: Vec<u8>,
    pub input: VecDeque<Key>,
}

static TASKS: Mutex<[Option<Task>; MAX_TASKS]> = Mutex::new([const { None }; MAX_TASKS]);
static mut KSTACKS: [[u8; KSTACK_SIZE]; MAX_TASKS] = [[0; KSTACK_SIZE]; MAX_TASKS];

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
        heap_break: 0,
        brk: 0,
        mmap_next: 0,
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
    let entry = process::load_image(pml4, elf)?;

    let top = kstack_top(index);
    let rsp = build_user_frame(top, entry, process::USER_STACK_TOP - 16);

    tasks[index] = Some(Task {
        name,
        kind: Kind::Native,
        pml4: pml4.as_u64(),
        kstack_top: top,
        rsp,
        done: false,
        heap_break: process::USER_HEAP_BASE,
        brk: process::USER_HEAP_BASE,
        mmap_next: 0,
        fs_base: 0,
        fds: new_fds(),
        output: Vec::new(),
        input: VecDeque::new(),
    });
    Ok(index)
}

/// Create a Linux task from a static ELF image. Returns its slot index.
pub fn spawn_linux(name: &'static str, elf: &[u8]) -> Result<usize, &'static str> {
    let mut tasks = TASKS.lock();
    let index = (1..MAX_TASKS)
        .find(|&i| tasks[i].is_none())
        .ok_or("no free task slot")?;

    let pml4 = mem::new_user_table().ok_or("out of memory")?;
    let (entry, stack_top) = process::linux::load(pml4, elf)?;

    let top = kstack_top(index);
    let rsp = build_user_frame(top, entry, stack_top);

    tasks[index] = Some(Task {
        name,
        kind: Kind::Linux,
        pml4: pml4.as_u64(),
        kstack_top: top,
        rsp,
        done: false,
        heap_break: 0,
        brk: process::linux::BRK_BASE,
        mmap_next: process::linux::MMAP_BASE,
        fs_base: 0,
        fds: new_fds(),
        output: Vec::new(),
        input: VecDeque::new(),
    });
    Ok(index)
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

/// Mark the current task finished.
pub fn finish_current() {
    let mut tasks = TASKS.lock();
    if let Some(task) = tasks[current()].as_mut() {
        task.done = true;
    }
    drop(tasks);
    NEEDS_REDRAW.store(true, Ordering::Relaxed);
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
            if !task.done {
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

/// Append output to the current task's terminal.
pub fn write_output(bytes: &[u8]) {
    let mut tasks = TASKS.lock();
    if let Some(task) = tasks[current()].as_mut() {
        task.output.extend_from_slice(bytes);
    }
    drop(tasks);
    NEEDS_REDRAW.store(true, Ordering::Relaxed);
}

/// Pop a key for the current task, if any.
pub fn take_key() -> Option<Key> {
    let mut tasks = TASKS.lock();
    tasks[current()]
        .as_mut()
        .and_then(|task| task.input.pop_front())
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

/// The current task's Linux `brk` break.
pub fn brk() -> u64 {
    TASKS.lock()[current()].as_ref().map(|t| t.brk).unwrap_or(0)
}

/// Set the current task's Linux `brk` break.
pub fn set_brk(value: u64) {
    if let Some(task) = TASKS.lock()[current()].as_mut() {
        task.brk = value;
    }
}

/// The current task's anonymous `mmap` bump pointer.
pub fn mmap_next() -> u64 {
    TASKS.lock()[current()]
        .as_ref()
        .map(|t| t.mmap_next)
        .unwrap_or(0)
}

/// Set the current task's anonymous `mmap` bump pointer.
pub fn set_mmap_next(value: u64) {
    if let Some(task) = TASKS.lock()[current()].as_mut() {
        task.mmap_next = value;
    }
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
