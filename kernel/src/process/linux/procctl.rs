//! Process and thread lifecycle: `clone`, `fork`, `execve`, `wait4`, `exit`,
//! `exit_group`, `set_tid_address`, and the process-group/session syscalls
//! (`setpgid`, `setsid`, `getpgid`, `getsid`) that ride along with them.

use alloc::vec::Vec;

use crate::fs::vfs::{self, FsError, Id};
use crate::mem::vma::{Kind, Prot};
use crate::process::{load_segments, map_range_kind};
use crate::task::process::GroupError;
use crate::task::{self, WakeReason};
use crate::user_ptr;

use super::elf::{build_start_stack, phdr_size, program_header_addr};
use super::errno::{
    err, fs_err, ECHILD, EFAULT, EINTR, EINVAL, ENOENT, ENOEXEC, ENOMEM, ENOSYS, EPERM, ESRCH,
};
use super::fd::close_cloexec_fds;
use super::futex::futex_wake;
use super::path::load_executable;
use super::uaccess::{read_cstr, write_u32, write_u64};
use super::{BRK_BASE, MMAP_BASE, STACK_SIZE, STACK_TOP};

/// Free task slots at which a successful `clone` gives the scheduler a tick
/// before returning, so a burst of thread creation cannot fill the table with
/// threads that have not run yet.
const CLONE_SLOT_RESERVE: usize = 4;

/// `clone` flags we honour (thread creation, and the `CLONE_VM`-without-
/// `CLONE_THREAD` vfork child musl's `posix_spawn` uses).
const CLONE_VM: u64 = 0x0000_0100;
const CLONE_SETTLS: u64 = 0x0008_0000;
const CLONE_PARENT_SETTID: u64 = 0x0010_0000;
const CLONE_CHILD_CLEARTID: u64 = 0x0020_0000;
const CLONE_THREAD: u64 = 0x0001_0000;

/// `clone(flags, stack, parent_tid, child_tid, tls)`.
///
/// `CLONE_VM|CLONE_THREAD` is a thread ([`task::spawn_thread`]); `CLONE_VM`
/// without `CLONE_THREAD` is musl `posix_spawn`'s vfork child
/// ([`task::spawn_vfork`]): shared address space, copied descriptor table,
/// inherited `%fs`. A plain `fork`-style clone (no `CLONE_VM`) is not modelled.
///
/// On a single CPU a parent can spawn threads faster than they run: a burst can
/// fill the table with threads that have not had a first quantum, and a spawn
/// that then fails would strand every one of them, because musl's
/// `pthread_create` holds the thread-list lock across `clone` (they cannot exit
/// until the call returns). So while the table is near capacity, a successful
/// clone sleeps for one tick after leaving the new thread runnable. The
/// scheduler then runs earlier threads, they exit, and their slots are
/// reclaimed (issue #133); a genuinely exhausted table still fails with
/// `ENOMEM` like Linux.
pub(super) fn sys_clone(flags: u64, stack: u64, parent_tid: u64, child_tid: u64, tls: u64) -> u64 {
    if flags & CLONE_VM == 0 {
        return err(ENOSYS); // fork/process creation is a later phase
    }
    let spawned = if flags & CLONE_THREAD != 0 {
        let fs_base = if flags & CLONE_SETTLS != 0 { tls } else { 0 };
        // `spawn_thread` also enforces this, but it reports `ENOMEM`; a bad
        // TLS pointer is the caller's error, so return `EINVAL` here (issue
        // #222). Reject before touching the task table.
        if !task::valid_fs_base(fs_base) {
            return err(EINVAL);
        }
        let clear = if flags & CLONE_CHILD_CLEARTID != 0 {
            child_tid
        } else {
            0
        };
        task::spawn_thread("thread", stack, fs_base, clear)
    } else {
        task::spawn_vfork(stack)
    };
    // Collect exits the scheduler already flagged before deciding the table is
    // under pressure.
    task::reclaim_pending();
    match spawned {
        Ok(index) => {
            if flags & CLONE_PARENT_SETTID != 0 && parent_tid != 0 {
                write_u64(parent_tid, index as u64);
            }
            if task::free_slots() <= CLONE_SLOT_RESERVE {
                task::wait_slot(task::ticks() + 1);
            }
            index as u64
        }
        Err(_) => err(ENOMEM),
    }
}

pub(super) fn sys_set_tid_address(tidptr: u64) -> u64 {
    task::set_clear_child_tid(tidptr);
    task::current() as u64
}

/// Read a NUL-terminated user string as raw bytes (including the terminator).
fn read_cstr_bytes(ptr: u64) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..4096u64 {
        // Safety: user memory up to the NUL terminator (the syscall ABI's contract).
        let byte = unsafe { user_ptr::read::<u8>(ptr + i) };
        out.push(byte);
        if byte == 0 {
            break;
        }
    }
    out
}

/// Read a NULL-terminated array of user string pointers.
fn read_str_ptr_array(arr: u64) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    if arr == 0 {
        return out;
    }
    for i in 0..256u64 {
        // Safety: user array of `char *` (the syscall ABI's contract).
        let ptr = unsafe { user_ptr::read_at::<u64>(arr, i as usize) };
        if ptr == 0 {
            break;
        }
        out.push(read_cstr_bytes(ptr));
    }
    out
}

/// Map special process paths to a real FAT entry (`/proc/self/exe` -> busybox).
fn resolve_exe(path: &str) -> &str {
    match path {
        "/proc/self/exe" => "busybox",
        other => other.trim_start_matches('/'),
    }
}

pub(super) fn sys_fork() -> u64 {
    match task::spawn_fork() {
        Ok(index) => index as u64,
        Err(_) => err(ENOMEM),
    }
}

pub(super) fn sys_wait4(_pid: u64, status: u64, options: u64) -> u64 {
    const WNOHANG: u64 = 1;
    if !task::has_children() {
        return err(ECHILD);
    }
    loop {
        if let Some((slot, code)) = task::reap_child() {
            if status != 0 {
                write_u32(status, (code as u32) << 8);
            }
            return slot as u64;
        }
        if options & WNOHANG != 0 {
            return 0; // no child has exited, don't wait
        }
        // No child is reapable yet: park until one exits. `sys_exit` (via
        // `finish_current`) notifies the queue, and the recheck above happens
        // with interrupts disabled, so an exit cannot slip in between.
        match task::wait_child_exit() {
            WakeReason::Woken | WakeReason::TimedOut => {}
            WakeReason::Interrupted => return err(EINTR),
        }
    }
}

/// `execve(path, argv, envp)`: replace the current image with `path`'s ELF and
/// resume at its entry point.
pub(super) fn sys_execve(path_ptr: u64, argv_ptr: u64, envp_ptr: u64) -> u64 {
    let Some(path) = read_cstr(path_ptr) else {
        return err(EFAULT);
    };
    let mut argv = read_str_ptr_array(argv_ptr);
    if argv.is_empty() {
        argv.push(read_cstr_bytes(path_ptr));
    }
    let envp = read_str_ptr_array(envp_ptr);

    let target = resolve_exe(&path);
    // Executables need the execute bit. Applet aliases and paths with no VFS
    // node fall through to `load_file`; root bypasses the check as usual.
    match crate::fs::abi_check(Id::current(), target, vfs::EXECUTE) {
        Ok(_) | Err(FsError::NotFound) => {}
        Err(error) => return fs_err(error),
    }
    let elf = match load_executable(target) {
        Ok(elf) => elf,
        Err(FsError::NotFound) => return err(ENOENT),
        Err(error) => return fs_err(error),
    };
    let Some(table) = crate::mem::new_user_table() else {
        return err(ENOMEM);
    };
    // Every error below returns while this guard is live, so a partially
    // loaded image cannot leak its address space and frames (issue #229).
    let guard = crate::mem::UserTableGuard::new(table);
    let entry = match load_segments(guard.table(), &elf) {
        Ok(entry) => entry,
        Err(_) => return err(ENOEXEC),
    };
    let stack = match map_range_kind(
        guard.table(),
        STACK_TOP - STACK_SIZE,
        STACK_TOP,
        Prot::READ | Prot::WRITE,
        Kind::Stack,
    ) {
        Ok(stack) => stack,
        Err(_) => return err(ENOMEM),
    };
    let phdr = program_header_addr(&elf);
    let (phent, phnum) = phdr_size(&elf);
    let rsp = build_start_stack(&stack, &argv, &envp, entry, phdr, phent, phnum);

    // The image is committed: close the descriptors std marked `O_CLOEXEC`
    // (the child's copies of the inherit-only pipe ends) before resuming.
    close_cloexec_fds();

    // Replace the process image: switch to the new table and make `sysretq`
    // resume at the new entry. Only now does the table outlive this call, so
    // disarm the guard after installing it.
    crate::mem::switch_to(table);
    task::set_pml4(table.as_u64());
    task::set_fs_base(0);
    task::register_bumps(table.as_u64(), BRK_BASE, MMAP_BASE);
    crate::arch::linux::set_user_return(entry, rsp, 0x202);
    guard.commit();
    0
}

/// Map a group/session error to its Linux errno.
fn group_err(error: GroupError) -> u64 {
    match error {
        GroupError::NoSuchProcess => err(ESRCH),
        GroupError::NotPermitted => err(EPERM),
        GroupError::Invalid => err(EINVAL),
    }
}

/// Unwrap a group/session syscall result, mapping errors to errno.
fn group_result(result: Result<usize, GroupError>) -> u64 {
    match result {
        Ok(value) => value as u64,
        Err(error) => group_err(error),
    }
}

/// `setpgid(pid, pgid)`: change the caller's group, or a child's.
pub(super) fn sys_setpgid(pid: u64, pgid: u64) -> u64 {
    match task::process::setpgid(task::current(), pid as i64, pgid as i64) {
        Ok(()) => 0,
        Err(error) => group_err(error),
    }
}

/// `setsid()`: start a new session and group with the caller as leader.
pub(super) fn sys_setsid() -> u64 {
    group_result(task::process::setsid(task::current()))
}

/// `getpgid(pid)`: the process group of `pid` (0 = the caller).
pub(super) fn sys_getpgid(pid: u64) -> u64 {
    group_result(task::process::getpgid(task::current(), pid as i64))
}

/// `getsid(pid)`: the session of `pid` (0 = the caller).
pub(super) fn sys_getsid(pid: u64) -> u64 {
    group_result(task::process::getsid(task::current(), pid as i64))
}

/// `exit_group(code)`: terminate the caller's *thread group* — every task that
/// shares its address space — each with `code`.
///
/// This fixes the #59 deviation, where `exit_group` killed the process group
/// (which would take forked children down with it): a process group is the job
/// control unit, the thread group is the process. Every thread's
/// `clear_child_tid` word is zeroed and futex-woken so joiners wake.
pub(super) fn sys_exit_group(code: u64) -> u64 {
    let tids = task::exit_thread_group(code & 0xff);
    for tid in tids {
        if tid != 0 {
            write_u32(tid, 0);
            futex_wake(tid, 1);
        }
    }
    loop {
        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
    }
}

pub(super) fn sys_exit(code: u64) -> u64 {
    // Thread exit: clear the TID word and wake anyone joining on it.
    let tid = task::clear_child_tid();
    if tid != 0 {
        write_u32(tid, 0);
        futex_wake(tid, 1);
    }
    task::finish_current(code & 0xff);
    loop {
        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
    }
}
