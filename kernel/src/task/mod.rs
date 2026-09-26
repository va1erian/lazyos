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

pub struct Task {
    pub name: &'static str,
    pub pml4: u64,
    pub kstack_top: u64,
    pub rsp: u64,
    pub done: bool,
    pub heap_break: u64,
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
        pml4: mem::kernel_table().as_u64(),
        kstack_top: 0,
        rsp: 0,
        done: false,
        heap_break: 0,
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
    let rsp = build_user_frame(top, entry);

    tasks[index] = Some(Task {
        name,
        pml4: pml4.as_u64(),
        kstack_top: top,
        rsp,
        done: false,
        heap_break: process::USER_HEAP_BASE,
        output: Vec::new(),
        input: VecDeque::new(),
    });
    Ok(index)
}

/// Lay out a fresh ring-3 entry frame on a kernel stack and return its RSP.
///
/// Layout (low to high) matches `timer_isr`'s pop order: 15 general registers,
/// then RIP, CS, RFLAGS, RSP, SS.
fn build_user_frame(kstack_top: u64, entry: u64) -> u64 {
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
        core::ptr::write_volatile(frame.add(18), process::USER_STACK_TOP - 16); // RSP
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
    let (pml4, kstack_top, rsp) = (task.pml4, task.kstack_top, task.rsp);
    drop(tasks);

    // Switch address space and the ring0 stack used for the next user trap.
    mem::switch_to(PhysAddr::new(pml4));
    if kstack_top != 0 {
        gdt::set_kernel_stack(kstack_top);
    }
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

/// Snapshot of a task's name, output and done flag, for rendering.
pub fn snapshot(index: usize) -> Option<(&'static str, Vec<u8>, bool)> {
    let tasks = TASKS.lock();
    tasks[index]
        .as_ref()
        .map(|task| (task.name, task.output.clone(), task.done))
}
