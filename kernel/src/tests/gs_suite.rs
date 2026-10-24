//! `%gs` against a hostile ring 3 (`arch::gsgate`).
//!
//! Ring 3 may load any DPL-3 selector into `%gs`, which sets its GS base to
//! that descriptor's base, 0. Before the `swapgs` discipline the kernel ran
//! every privilege level on one GS base, so the next `syscall` saved the user
//! context through `gs:` at low *user* addresses and took its stack pointer
//! from there. A real ring-3 task here zeroes its GS base, then crosses every
//! kind of gate many times — Linux `syscall`, native `int 0x80` and the
//! timer preempting it mid-spin — and after each round reads `gs:[CODE]`,
//! which equals `[CODE]` only while its GS base is still its own 0 (a kernel
//! base leaking back would fault or differ). Another task faults with that
//! base (`int3` is `#GP` from ring 3: the gate is DPL 0), so the paranoid
//! exception entry must find the kernel's GS to end it. Any entry that
//! trusted the user's base would fault in ring 0.

use super::*;
use crate::mem::vma::{Kind as VmaKind, Prot};

/// Where the task's code is mapped in its own address space.
const CODE: u64 = 0x0060_0000;
/// Where in that page a finished task writes its result, then parks: tasks
/// the kernel task forked are parentless, so a sibling's next syscall would
/// reclaim an exited one before its status could be read.
const RESULT: usize = 0xF00;
/// The result that says every round trip worked.
pub(super) const PASSED: u32 = 42;
/// The result of a task whose `%gs` selector changed under it.
const SELECTOR_LOST: u32 = 7;
/// The result of a task whose GS base was not its own after a round.
const BASE_LOST: u32 = 9;
/// The exit status of a task ended by a fault (`SIGSEGV`, 128 + 11).
const KILLED: u64 = 139;

/// The program, with the user data selector `sel` and the round count
/// patched in: load `sel` into `%gs`, then (`fault`) an `int3`, which ends
/// the task through `#GP`, or a `nop`; then `rounds` rounds of `getpid`
/// (`syscall`), native `clock` (`int 0x80`) and a 20000-`pause` spin the
/// timer preempts, each checked by `gs:[CODE] == [CODE]`; then 42 (or 7
/// when `%gs` no longer reads `sel`, 9 when a round's check failed) is
/// written at [`RESULT`] and the task spins on `pause`.
#[rustfmt::skip]
fn program(sel: u16, rounds: u32, fault: bool) -> Vec<u8> {
    let [lo, hi] = sel.to_le_bytes();
    let [r0, r1, r2, r3] = rounds.to_le_bytes();
    let first = if fault { 0xcc } else { 0x90 };
    vec![
        0x66, 0xb8, lo, hi, //                             0x00 mov ax, sel
        0x8e, 0xe8, //                                     0x04 mov gs, eax
        first, //                                          0x06 int3 (#GP) or nop
        0xbb, r0, r1, r2, r3, //                           0x07 mov ebx, rounds
        0xb8, 0x27, 0x00, 0x00, 0x00, //                   0x0c mov eax, 39 (getpid)
        0x0f, 0x05, //                                     0x11 syscall
        0xb8, 0x08, 0x00, 0x00, 0x00, //                   0x13 mov eax, 8 (native clock)
        0xcd, 0x80, //                                     0x18 int 0x80
        0xb9, 0x20, 0x4e, 0x00, 0x00, //                   0x1a mov ecx, 20000
        0xf3, 0x90, //                                     0x1f pause
        0xff, 0xc9, //                                     0x21 dec ecx
        0x75, 0xfa, //                                     0x23 jnz 0x1f
        0x65, 0x8b, 0x04, 0x25, 0x00, 0x00, 0x60, 0x00, // 0x25 mov eax, gs:[CODE]
        0x3b, 0x04, 0x25, 0x00, 0x00, 0x60, 0x00, //       0x2d cmp eax, [CODE]
        0x75, 0x1a, //                                     0x34 jne 0x50 (base lost)
        0xff, 0xcb, //                                     0x36 dec ebx
        0x75, 0xd2, //                                     0x38 jnz 0x0c
        0x8c, 0xe8, //                                     0x3a mov eax, gs
        0x66, 0x3d, lo, hi, //                             0x3c cmp ax, sel
        0x75, 0x07, //                                     0x40 jne 0x49 (selector lost)
        0xbf, 0x2a, 0x00, 0x00, 0x00, //                   0x42 mov edi, 42
        0xeb, 0x0c, //                                     0x47 jmp 0x55
        0xbf, 0x07, 0x00, 0x00, 0x00, //                   0x49 mov edi, 7
        0xeb, 0x05, //                                     0x4e jmp 0x55
        0xbf, 0x09, 0x00, 0x00, 0x00, //                   0x50 mov edi, 9
        0x89, 0x3c, 0x25, 0x00, 0x0f, 0x60, 0x00, //       0x55 mov [CODE + RESULT], edi
        0xf3, 0x90, //                                     0x5c pause
        0xeb, 0xfc, //                                     0x5e jmp 0x5c
    ]
}

/// A forked hostile task and its code page, read through the physical map.
pub(super) struct Hostile {
    pub(super) slot: usize,
    page: *const u8,
}

impl Hostile {
    /// What the task wrote at [`RESULT`] (0 until it finished).
    /// The 64-bit word at [`RESULT`] (a counting program's count).
    pub(super) fn count(&self) -> u64 {
        // SAFETY: as `result`; `RESULT` is 8-aligned.
        unsafe { (self.page.add(RESULT) as *const u64).read_volatile() }
    }

    pub(super) fn result(&self) -> u32 {
        // SAFETY: `page` is the task's code frame through the physical map,
        // alive until the task is ended; `RESULT` is inside the page and
        // 4-aligned. Volatile: ring 3 writes it.
        unsafe { (self.page.add(RESULT) as *const u32).read_volatile() }
    }
}

/// A forked task that runs [`program`] from ring 3 when next scheduled.
pub(super) fn spawn_hostile(rounds: u32, fault: bool) -> Result<Hostile, String> {
    spawn_user(&program(
        crate::arch::gdt::selectors().user_data,
        rounds,
        fault,
    ))
}

/// A forked task that runs `code` (at most `RESULT` bytes, position
/// independent, mapped at `CODE`) from ring 3 when next scheduled.
pub(super) fn spawn_user(code: &[u8]) -> Result<Hostile, String> {
    let slot = task::spawn_fork().map_err(|error| format!("fork: {error}"))?;
    let pml4 = x86_64::PhysAddr::new(task::harness::pml4(slot).ok_or("the task has no table")?);
    let pages = process::map_range_kind(
        pml4,
        CODE,
        CODE + 4096,
        Prot::READ | Prot::WRITE | Prot::EXEC,
        VmaKind::Anon,
    )
    .map_err(|error| format!("map the program: {error}"))?;
    let (_, phys) = *pages.first().ok_or("no code page")?;
    check!(
        code.len() <= RESULT,
        "a {}-byte program overlaps its result",
        code.len()
    );
    let page = mem::phys_to_virt(x86_64::PhysAddr::new(phys)).as_mut_ptr::<u8>();
    // SAFETY: `phys` is the frame `map_range_kind` just allocated for the
    // task, reached through the physical-memory map; the program and the
    // result word are inside the page.
    unsafe {
        core::ptr::write_bytes(page, 0, 4096);
        core::ptr::copy_nonoverlapping(code.as_ptr(), page, code.len());
    }
    check!(
        task::harness::set_user_frame(slot, CODE, CODE + 4096),
        "could not point the task at its program"
    );
    Ok(Hostile { slot, page })
}

/// The kernel task, fresh and runnable, as the tasks' parent.
fn fresh() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
}

/// Run `tasks` (with user pointers checked for real: the suite trusts kernel
/// pointers by default, and these are ring-3 tasks) until each wrote its
/// result; each must be [`PASSED`] and must have been charged a timer tick
/// (the timer interrupted it in ring 3, where it spends its time). The tasks
/// are then ended and reclaimed.
fn run_parked(tasks: &[Hostile]) -> Result<(), String> {
    let trusted = crate::user_ptr::set_trust_kernel_pointers(false);
    let outcome = wait_results(tasks);
    for hostile in tasks {
        if let Some(pml4) = task::harness::pml4(hostile.slot) {
            crate::task::signal::terminate_process(pml4, 0);
        }
    }
    task::reclaim_pending();
    crate::user_ptr::set_trust_kernel_pointers(trusted);
    outcome
}

fn wait_results(tasks: &[Hostile]) -> Result<(), String> {
    let mut ticked = alloc::vec![false; tasks.len()];
    for _ in 0..20_000 {
        task::wait_sleep(task::ticks() + 1);
        for (i, hostile) in tasks.iter().enumerate() {
            ticked[i] |= task::cpu_ticks(hostile.slot) > 0;
        }
        let results: Vec<u32> = tasks.iter().map(Hostile::result).collect();
        if results.iter().all(|&r| r != 0) {
            for (hostile, &result) in tasks.iter().zip(&results) {
                let slot = hostile.slot;
                check!(
                    result != SELECTOR_LOST,
                    "task {slot}: its %gs selector changed across a kernel entry"
                );
                check!(
                    result != BASE_LOST,
                    "task {slot}: its GS base was not its own after a round trip"
                );
                check!(
                    result == PASSED,
                    "task {slot} wrote {result} (wanted {PASSED})"
                );
            }
            check!(
                ticked.iter().all(|&t| t),
                "a task was never charged a tick (the timer gate from ring 3 was not crossed)"
            );
            return Ok(());
        }
    }
    let results: Vec<u32> = tasks.iter().map(Hostile::result).collect();
    Err(format!(
        "the hostile tasks did not all finish (results {results:?})"
    ))
}

/// One hostile task crosses every gate with a zero GS base.
pub fn hostile_user_gs_survives_every_gate() -> Result<(), String> {
    fresh();
    run_parked(&[spawn_hostile(400, false)?])
}

/// A task with a zero GS base takes a fault (`#GP`): the paranoid entry must
/// swap the kernel's base in to end it, and the kernel goes on.
pub fn hostile_user_gs_fault_is_contained() -> Result<(), String> {
    fresh();
    let hostile = spawn_hostile(1, true)?;
    let trusted = crate::user_ptr::set_trust_kernel_pointers(false);
    let mut status = None;
    for _ in 0..2000 {
        task::wait_sleep(task::ticks() + 1);
        status = task::reap_child_slot(hostile.slot);
        if status.is_some() {
            break;
        }
    }
    crate::user_ptr::set_trust_kernel_pointers(trusted);
    check!(
        status == Some(KILLED),
        "the faulting task ended with {status:?} (wanted {KILLED})"
    );
    Ok(())
}

/// Six hostile tasks at once, preempting one another in ring 3 with zero GS
/// bases: every switch between them crosses the timer gate both ways.
pub fn hostile_user_gs_soak_many_tasks() -> Result<(), String> {
    fresh();
    let mut tasks = Vec::new();
    for _ in 0..6 {
        tasks.push(spawn_hostile(150, false)?);
    }
    run_parked(&tasks)
}

/// The paranoid gate's in-window branch, from ring 0: with `IA32_GS_BASE`
/// = 0 and the area in `IA32_KERNEL_GS_BASE` (exactly the state inside an
/// exit window), an `int3` must swap the area in for its handler and swap
/// back on the way out, leaving both MSRs as they were.
pub fn paranoid_gate_restores_a_window_state() -> Result<(), String> {
    const GS_BASE: u32 = 0xC000_0101;
    const KERNEL_GS_BASE: u32 = 0xC000_0102;
    let area = crate::arch::msr::read(GS_BASE);
    let saved_kernel = crate::arch::msr::read(KERNEL_GS_BASE);
    let (after_gs, after_kernel): (u64, u64);
    // SAFETY: interrupts are off (the suite runs with them off) and no Rust
    // runs between the writes: the asm block puts the window state in place,
    // takes the breakpoint (whose paranoid gate must swap the area in for its
    // handler and back), reads both MSRs and restores them before returning.
    unsafe {
        core::arch::asm!(
            "mov ecx, {gs}",
            "xor eax, eax",
            "xor edx, edx",
            "wrmsr",                      // GS_BASE = 0 (a user's)
            "mov ecx, {kgs}",
            "mov rax, {area}",
            "mov rdx, {area}",
            "shr rdx, 32",
            "wrmsr",                      // KERNEL_GS_BASE = the area
            "int3",
            "mov ecx, {gs}",
            "rdmsr",
            "shl rdx, 32",
            "or rax, rdx",
            "mov {after_gs}, rax",
            "mov ecx, {kgs}",
            "rdmsr",
            "shl rdx, 32",
            "or rax, rdx",
            "mov {after_kernel}, rax",
            "mov ecx, {gs}",              // put the kernel's state back
            "mov rax, {area}",
            "mov rdx, {area}",
            "shr rdx, 32",
            "wrmsr",
            "mov ecx, {kgs}",
            "mov rax, {saved}",
            "mov rdx, {saved}",
            "shr rdx, 32",
            "wrmsr",
            gs = const GS_BASE,
            kgs = const KERNEL_GS_BASE,
            area = in(reg) area,
            saved = in(reg) saved_kernel,
            after_gs = out(reg) after_gs,
            after_kernel = out(reg) after_kernel,
            out("rax") _,
            out("rcx") _,
            out("rdx") _,
        );
    }
    check!(
        after_gs == 0,
        "the gate left IA32_GS_BASE = {after_gs:#x} (wanted the window's 0)"
    );
    check!(
        after_kernel == area,
        "the gate left IA32_KERNEL_GS_BASE = {after_kernel:#x} (wanted the area {area:#x})"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "gs_hostile_user_gs_survives_every_gate",
        hostile_user_gs_survives_every_gate,
    ),
    (
        "gs_hostile_user_gs_fault_is_contained",
        hostile_user_gs_fault_is_contained,
    ),
    (
        "gs_hostile_user_gs_soak_many_tasks",
        hostile_user_gs_soak_many_tasks,
    ),
    (
        "gs_paranoid_gate_restores_a_window_state",
        paranoid_gate_restores_a_window_state,
    ),
];
