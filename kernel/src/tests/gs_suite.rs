//! `%gs` against a hostile ring 3 (`arch::gsgate`).
//!
//! Ring 3 may load any DPL-3 selector into `%gs`, which sets its GS base to
//! that descriptor's base, 0. Before the `swapgs` discipline the kernel ran
//! every privilege level on one GS base, so the next `syscall` saved the user
//! context through `gs:` at low *user* addresses and took its stack pointer
//! from there. A real ring-3 task here zeroes its GS base, then crosses every
//! kind of gate many times — Linux `syscall`, native `int 0x80`, and the
//! timer preempting it mid-spin — and exits 42 only if it got back each time
//! with its own `%gs` selector intact. Any entry that trusted the user's base
//! would fault in ring 0 instead.

use super::*;
use crate::mem::vma::{Kind as VmaKind, Prot};

/// Where the task's code is mapped in its own address space.
const CODE: u64 = 0x0060_0000;
/// The exit status that says every round trip worked.
const PASSED: u64 = 42;
/// The exit status of a task whose `%gs` selector changed under it.
const SELECTOR_LOST: u64 = 7;

/// The program, with the user data selector `sel` patched in: load it into
/// `%gs`, then 400 rounds of `getpid` (`syscall`), native `clock`
/// (`int 0x80`) and a 20000-`pause` spin the timer preempts, then
/// `exit_group(42)` if `%gs` still reads `sel`, else `exit_group(7)`.
fn program(sel: u16) -> Vec<u8> {
    let [lo, hi] = sel.to_le_bytes();
    vec![
        0x66, 0xb8, lo, hi, //             0x00 mov ax, sel
        0x8e, 0xe8, //                     0x04 mov gs, eax
        0xbb, 0x90, 0x01, 0x00, 0x00, //   0x06 mov ebx, 400
        0xb8, 0x27, 0x00, 0x00, 0x00, //   0x0b mov eax, 39 (getpid)
        0x0f, 0x05, //                     0x10 syscall
        0xb8, 0x08, 0x00, 0x00, 0x00, //   0x12 mov eax, 8 (native clock)
        0xcd, 0x80, //                     0x17 int 0x80
        0xb9, 0x20, 0x4e, 0x00, 0x00, //   0x19 mov ecx, 20000
        0xf3, 0x90, //                     0x1e pause
        0xff, 0xc9, //                     0x20 dec ecx
        0x75, 0xfa, //                     0x22 jnz 0x1e
        0xff, 0xcb, //                     0x24 dec ebx
        0x75, 0xe3, //                     0x26 jnz 0x0b
        0x8c, 0xe8, //                     0x28 mov eax, gs
        0xbf, 0x2a, 0x00, 0x00, 0x00, //   0x2a mov edi, 42
        0x66, 0x3d, lo, hi, //             0x2f cmp ax, sel
        0x74, 0x05, //                     0x33 je 0x3a
        0xbf, 0x07, 0x00, 0x00, 0x00, //   0x35 mov edi, 7
        0xb8, 0xe7, 0x00, 0x00, 0x00, //   0x3a mov eax, 231 (exit_group)
        0x0f, 0x05, //                     0x3f syscall
    ]
}

/// A forked task that runs [`program`] from ring 3 when next scheduled.
fn spawn_hostile() -> Result<usize, String> {
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
    let code = program(crate::arch::gdt::selectors().user_data);
    let page = mem::phys_to_virt(x86_64::PhysAddr::new(phys)).as_mut_ptr::<u8>();
    // SAFETY: `phys` is the frame `map_range_kind` just allocated for the
    // task, reached through the physical-memory map; the program is under a
    // page.
    unsafe { core::ptr::copy_nonoverlapping(code.as_ptr(), page, code.len()) };
    check!(
        task::harness::set_user_frame(slot, CODE, CODE + 4096),
        "could not point the task at its program"
    );
    Ok(slot)
}

/// The hostile task crosses every gate with a zero GS base and exits 42.
pub fn hostile_user_gs_survives_every_gate() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
    let ticks = task::ticks();
    let slot = spawn_hostile()?;
    for _ in 0..4000 {
        task::wait_sleep(task::ticks() + 1);
        if let Some(status) = task::reap_child_slot(slot) {
            check!(
                status != SELECTOR_LOST,
                "the task's %gs selector changed across a kernel entry"
            );
            check!(
                status == PASSED,
                "the hostile task exited {status} (wanted {PASSED})"
            );
            check!(
                task::ticks() > ticks + 1,
                "no timer tick arrived while the task ran (the IRQ gate was not crossed)"
            );
            return Ok(());
        }
    }
    Err(format!("the hostile task {slot} did not exit"))
}

pub(super) const CASES: &[(&str, Test)] = &[(
    "gs_hostile_user_gs_survives_every_gate",
    hostile_user_gs_survives_every_gate,
)];
