//! Spawn, descriptor/argument inheritance, reaping, failure paths and the `&`
//! lifecycle of `execve`d native programs (see the parent module).

use super::*;
use crate::ipc::pipe;
use crate::task::FdKind;

const SYS_READ: u64 = 0;
const SYS_PIPE: u64 = 22;
const SYS_DUP2: u64 = 33;
const SYS_WAIT4: u64 = 61;
const SYS_EXECVE: u64 = 59;

const ENOENT: i64 = 2;
const ENOEXEC: u64 = 8;
const EAGAIN: u64 = 11;

/// The spawned program is a child of the caller, gets the caller's
/// descriptors (but not `FD_CLOEXEC` ones), its argument string, and its exit
/// status is what the waiting caller reads; the slot and address space are
/// released by that reap.
pub fn spawn_inherits_fds_args_and_reports_status() -> Result<(), String> {
    fresh();
    let sh = shell()?;
    let mut fds = [0i32; 2];
    let ret = process::linux::dispatch_for_test(SYS_PIPE, fds.as_mut_ptr() as u64, 0, 0);
    check!(ret == 0, "pipe returned {ret:#x}");
    let (read_end, write_end) = (fds[0] as usize, fds[1] as usize);
    // `sh` semantics for `cmd > pipe`: the write end becomes fd 1, the spare
    // read end is close-on-exec.
    let ret = process::linux::dispatch_for_test(SYS_DUP2, write_end as u64, 1, 0);
    check!(ret == 1, "dup2 returned {ret:#x}");
    check!(
        task::fd_set_cloexec(read_end, true),
        "could not mark cloexec"
    );

    let frames_before = mem::frame_stats().live();
    let child = native::spawn("TOP.ELF", &service_suite::minimal_elf(), "-a x")
        .map_err(|e| format!("spawn errno {e}"))?;
    check!(
        task::process::ppid_of(child) == sh,
        "native child's ppid is {} (expected the caller {sh})",
        task::process::ppid_of(child)
    );
    check!(task::is_child(child), "is_child() is false for the child");
    check!(
        task::harness::fd_kind_at(child, 0) == FdKind::Terminal,
        "stdin not inherited"
    );
    check!(
        task::harness::fd_kind_at(child, 1) == FdKind::Pipe,
        "stdout did not follow the caller's redirection"
    );
    check!(
        task::harness::fd_kind_at(child, read_end) == FdKind::Closed,
        "an FD_CLOEXEC descriptor leaked into the native child"
    );
    check!(
        task::harness::fd_kind_at(child, write_end) == FdKind::Pipe,
        "an ordinary descriptor was not inherited"
    );

    // Its argument string reaches it as the `argv` block syscall 9 returns:
    // the task name, then the split arguments.
    task::harness::switch_current(child);
    let mut buf = [0u8; 32];
    let len = process::dispatch_for_test(9, buf.as_mut_ptr() as u64, buf.len() as u64, 0);
    let want = b"TOP.ELF\0-a\0x\0";
    check!(
        len == want.len() as u64 && &buf[..want.len()] == want,
        "native argv block was {len} bytes: {:?}",
        &buf[..(len as usize).min(32)]
    );
    // Its output follows fd 1 into the pipe instead of the terminal.
    let text = b"hello\n";
    let wrote = process::dispatch_for_test(1, text.as_ptr() as u64, text.len() as u64, 0);
    check!(
        wrote == text.len() as u64,
        "native write returned {wrote:#x}"
    );
    task::harness::switch_current(sh);
    let mut got = [0u8; 16];
    let n =
        process::linux::dispatch_for_test(SYS_READ, read_end as u64, got.as_mut_ptr() as u64, 16);
    check!(
        n == 6 && &got[..6] == text,
        "the pipe read returned {n:#x} / {:?}",
        &got[..6]
    );

    check!(
        task::reap_child_slot(child).is_none(),
        "a running child was reaped"
    );
    task::harness::finish(child, 42);
    check!(
        task::reap_child_slot(child) == Some(42),
        "the exit status did not propagate"
    );
    check!(
        task::reap_child_slot(child).is_none() && !task::is_child(child),
        "the child was reaped twice"
    );
    check!(
        task::snapshot(child).is_none(),
        "the child's slot was not freed"
    );
    check!(
        mem::frame_stats().live() <= frames_before,
        "the native child's address space leaked ({} -> {})",
        frames_before,
        mem::frame_stats().live()
    );
    check!(
        task::fd_close(read_end) && task::fd_close(write_end) && task::fd_close(1),
        "cleanup failed"
    );
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::reset();
    check!(
        pipe::Pipe::live() == 0,
        "{} pipes leaked",
        pipe::Pipe::live()
    );
    Ok(())
}

/// Only the awaited child is reaped, and a task that is not the parent cannot
/// reap it.
pub fn reap_is_specific_to_the_child() -> Result<(), String> {
    fresh();
    let sh = shell()?;
    let first = native::spawn("TOP.ELF", &service_suite::minimal_elf(), "")
        .map_err(|e| format!("spawn errno {e}"))?;
    let second = native::spawn("CONFCTL.ELF", &service_suite::minimal_elf(), "")
        .map_err(|e| format!("spawn errno {e}"))?;
    task::harness::finish(first, 1);
    task::harness::finish(second, 2);
    check!(
        task::reap_child_slot(second) == Some(2),
        "the wrong child was reaped for the second slot"
    );
    check!(
        task::reap_child_slot(second).is_none(),
        "a reaped slot reaped again"
    );
    // The first child is still waiting for its parent to collect it.
    check!(
        task::is_child(first),
        "the unrelated finished child vanished"
    );
    task::harness::switch_current(task::KERNEL_TASK);
    check!(
        task::reap_child_slot(first).is_none(),
        "a task that is not the parent reaped the child"
    );
    task::harness::switch_current(sh);
    check!(
        task::reap_child_slot(first) == Some(1),
        "the parent could not reap its finished child"
    );
    task::harness::reset();
    Ok(())
}

/// Failure paths report the right errno and release everything: a corrupt
/// image is `ENOEXEC`, a full task table `EAGAIN`, an image that is not on the
/// boot volume `ENOENT` through the real `execve` entry point.
pub fn failures_report_errno_without_leaking() -> Result<(), String> {
    fresh();
    shell()?;
    let frames_before = mem::frame_stats().live();
    let slots_before = task::free_slots();
    for attempt in 0..8 {
        let result = native::spawn("TOP.ELF", b"this is not an ELF image", "");
        check!(
            result == Err(ENOEXEC),
            "attempt {attempt}: corrupt image gave {result:?}, expected ENOEXEC"
        );
    }
    check!(
        task::free_slots() == slots_before,
        "a refused image consumed a task slot"
    );
    check!(
        mem::frame_stats().live() == frames_before,
        "refused images leaked {} frames",
        mem::frame_stats().live() as i64 - frames_before as i64
    );

    // The harness boots without a FAT volume, so the named file is missing.
    let path = b"/bin/top\0";
    let code = process::linux::dispatch_for_test(SYS_EXECVE, path.as_ptr() as u64, 0, 0);
    check!(
        code == failed(ENOENT),
        "execve of a program missing from the volume -> {code:#x}, expected -ENOENT"
    );

    // Fill the table: the next spawn is EAGAIN, and freeing one slot fixes it.
    let mut fillers = Vec::new();
    while let Ok(slot) = task::spawn_child("fill", &service_suite::minimal_elf()) {
        fillers.push(slot);
    }
    check!(task::free_slots() == 0, "the table did not fill");
    let result = native::spawn("TOP.ELF", &service_suite::minimal_elf(), "");
    check!(
        result == Err(EAGAIN),
        "a full table gave {result:?}, expected EAGAIN"
    );
    let last = fillers.pop().ok_or("no filler tasks")?;
    task::harness::finish(last, 0);
    check!(
        task::reap_child_slot(last) == Some(0),
        "could not free a slot"
    );
    let slot = native::spawn("TOP.ELF", &service_suite::minimal_elf(), "")
        .map_err(|e| format!("spawn after freeing a slot: errno {e}"))?;
    check!(task::is_child(slot), "the retry did not start a child");
    // Reap everything through the parent so the address spaces are released
    // (`harness::reset` only drops the task entries).
    fillers.push(slot);
    for slot in fillers {
        task::harness::finish(slot, 0);
        check!(
            task::reap_child_slot(slot).is_some(),
            "could not reap {slot}"
        );
    }
    check!(
        mem::frame_stats().live() <= frames_before,
        "filling the table leaked frames: {frames_before} -> {}",
        mem::frame_stats().live()
    );
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::reset();
    Ok(())
}

/// `top &`: the shell forks a child, the child runs the native program and
/// waits for it, the shell keeps going without waiting and collects the
/// forked child (and the program's status) later with `wait4`. Nothing may
/// linger afterwards.
pub fn background_program_is_reaped_through_the_shell() -> Result<(), String> {
    fresh();
    let sh = shell()?;
    let slots_before = task::free_slots();
    let frames_before = mem::frame_stats().live();

    // The forked child (shell -> P) runs the program (P -> N).
    let forked = task::spawn_child("fork", &service_suite::minimal_elf()).map_err(to_string)?;
    task::harness::switch_current(forked);
    let program = native::spawn("TOP.ELF", &service_suite::minimal_elf(), "")
        .map_err(|e| format!("spawn errno {e}"))?;
    task::harness::switch_current(sh);

    // The prompt is back: the shell has not blocked, does not own N, and
    // `wait4(-1, WNOHANG)` finds nothing to collect while N still runs.
    check!(
        !task::is_child(program),
        "the shell owns the program directly"
    );
    let mut status = 0u32;
    let ret = process::linux::dispatch_args_for_test(
        SYS_WAIT4,
        u64::MAX,
        &mut status as *mut u32 as u64,
        1,
        0,
    );
    check!(ret == 0, "WNOHANG wait4 with a live job returned {ret:#x}");

    // The program exits and its waiting parent forwards the status.
    task::harness::finish(program, 7);
    task::harness::switch_current(forked);
    let code = task::reap_child_slot(program).ok_or("the program was not reapable")?;
    check!(code == 7, "program status {code}");
    task::harness::finish(forked, code);
    task::harness::switch_current(sh);

    // `wait` in the shell returns the program's status via the forked child.
    let ret = process::linux::dispatch_args_for_test(
        SYS_WAIT4,
        u64::MAX,
        &mut status as *mut u32 as u64,
        0,
        0,
    );
    check!(
        ret == forked as u64,
        "wait4 returned {ret:#x}, expected {forked}"
    );
    check!(
        status >> 8 == 7,
        "wait4 status {status:#x}, expected exit code 7"
    );
    check!(
        task::free_slots() == slots_before,
        "slots leaked: {} free, {} before",
        task::free_slots(),
        slots_before
    );
    check!(
        mem::frame_stats().live() <= frames_before,
        "frames leaked: {} -> {}",
        frames_before,
        mem::frame_stats().live()
    );
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::reset();
    Ok(())
}
