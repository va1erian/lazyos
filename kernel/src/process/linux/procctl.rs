//! Process and thread lifecycle: `clone`, `fork`, `execve`, `wait4`, `exit`,
//! `exit_group`, `set_tid_address`, and the process-group/session syscalls
//! (`setpgid`, `setsid`, `getpgid`, `getsid`) that ride along with them.

use alloc::vec::Vec;

use crate::task::process::GroupError;
use crate::task::{self, ThreadShare};
use crate::user_ptr;

use super::cwd::{read_path, resolve_at, AT_FDCWD};
use super::elf::load_image;
use super::errno::{err, EINVAL, ENOMEM, ENOSYS, EPERM, ESRCH};
use super::fd::close_cloexec_fds;
use super::futex::futex_wake;
use super::uaccess::write_u32;
use super::MMAP_BASE;

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
const CLONE_FS: u64 = 0x0000_0200;
const CLONE_FILES: u64 = 0x0000_0400;
const CLONE_SIGHAND: u64 = 0x0000_0800;
const CLONE_VFORK: u64 = 0x0000_4000;
const CLONE_SYSVSEM: u64 = 0x0004_0000;
const CLONE_DETACHED: u64 = 0x0040_0000;
const CLONE_CHILD_SETTID: u64 = 0x0100_0000;
/// The exit signal a process-style clone sends its parent (low byte).
const CSIGNAL: u64 = 0xff;

/// Flags whose effect is modelled (or is a no-op here for a documented
/// reason): anything else is refused with `EINVAL` rather than ignored.
/// `CLONE_SIGHAND` is implied (handlers are per address space), `CLONE_SYSVSEM`
/// has nothing to share (no System V semaphores), `CLONE_DETACHED` is ignored
/// by Linux itself, and `CLONE_VFORK`'s "parent waits" is what musl's
/// `posix_spawn` already does through its status pipe.
const CLONE_KNOWN: u64 = CSIGNAL
    | CLONE_VM
    | CLONE_FS
    | CLONE_FILES
    | CLONE_SIGHAND
    | CLONE_VFORK
    | CLONE_THREAD
    | CLONE_SYSVSEM
    | CLONE_SETTLS
    | CLONE_PARENT_SETTID
    | CLONE_CHILD_CLEARTID
    | CLONE_DETACHED
    | CLONE_CHILD_SETTID;

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
    if flags & !CLONE_KNOWN != 0 {
        // Logged as the errno the caller gets (`EINVAL`), so the ABI coverage
        // summary does not count a rejected flag as an unimplemented syscall.
        crate::serial_println!("EINVAL 56 clone unknown flags {:#x}", flags & !CLONE_KNOWN);
        return err(EINVAL);
    }
    if flags & CLONE_VM == 0 {
        // A process-style clone (`fork` spelled as `clone(SIGCHLD)`): the
        // child is a copy-on-write copy, so a `CLONE_CHILD_*` word would have
        // to be written into the child's copy, which is not modelled.
        if flags & (CLONE_CHILD_SETTID | CLONE_CHILD_CLEARTID | CLONE_THREAD | CLONE_SETTLS) != 0
            || stack != 0
        {
            return err(ENOSYS);
        }
        let pid = sys_fork();
        if flags & CLONE_PARENT_SETTID != 0 && parent_tid != 0 && (pid as i64) > 0 {
            write_u32(parent_tid, pid as u32);
        }
        return pid;
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
        let share = ThreadShare {
            files: flags & CLONE_FILES != 0,
            fs: flags & CLONE_FS != 0,
        };
        task::spawn_thread_sharing("thread", stack, fs_base, clear, share)
    } else {
        task::spawn_vfork(stack)
    };
    // Collect exits the scheduler already flagged before deciding the table is
    // under pressure.
    task::reclaim_pending();
    match spawned {
        Ok(index) => {
            // `pid_t` is 32 bits: a wider store would clobber the field
            // after musl's `tid`.
            if flags & CLONE_PARENT_SETTID != 0 && parent_tid != 0 {
                write_u32(parent_tid, index as u32);
            }
            // The address space is shared, so the child's word is ours.
            if flags & CLONE_CHILD_SETTID != 0 && child_tid != 0 {
                write_u32(child_tid, index as u32);
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

pub(super) fn sys_fork() -> u64 {
    match task::spawn_fork() {
        Ok(index) => index as u64,
        Err(_) => err(ENOMEM),
    }
}

/// `execve(path, argv, envp)`: replace the current image with `path`'s ELF (or,
/// for a `#!` script, its interpreter's) and resume at its entry point.
pub(super) fn sys_execve(path_ptr: u64, argv_ptr: u64, envp_ptr: u64) -> u64 {
    let raw = match read_path(path_ptr) {
        Ok(raw) => raw,
        Err(code) => return code,
    };
    // A relative program path is relative to the caller's working directory;
    // the cwd itself carries over into the new image.
    let path = match resolve_at(AT_FDCWD, &raw) {
        Ok(path) => path,
        Err(code) => return code,
    };
    let mut argv = read_str_ptr_array(argv_ptr);
    if argv.is_empty() {
        argv.push(read_cstr_bytes(path_ptr));
    }
    let envp = read_str_ptr_array(envp_ptr);

    // A native LazyOS program (`top`, `confctl`, ...) is run as a child rather
    // than loaded over this image (issue #315).
    if let Some(code) = super::native::try_exec(&path, &argv) {
        return code;
    }

    // A `#!` script runs its interpreter instead (issue #491); `image` is the
    // ELF at the end of that chain, with argv rewritten for it.
    let image = match super::shebang::resolve(path, raw.into_bytes(), argv) {
        Ok(image) => image,
        Err(code) => return code,
    };
    let Some(table) = crate::mem::new_user_table() else {
        return err(ENOMEM);
    };
    // Every error below returns while this guard is live, so a partially
    // loaded image cannot leak its address space and frames (issue #229).
    let guard = crate::mem::UserTableGuard::new(table);
    // The file is streamed into the new address space, never held whole.
    let ids = super::creds::ids();
    let started = match load_image(guard.table(), &image.file, &image.argv, &envp, ids) {
        Ok(started) => started,
        Err(error) => return err(error.errno()),
    };

    // The image is committed. Record the program (`/proc/self/exe`), which
    // also stops sharing the descriptor table with the old threads, then close
    // the descriptors std marked `O_CLOEXEC` (the child's copies of the
    // inherit-only pipe ends) before resuming.
    task::linuxstate::set_exe(&image.path);
    close_cloexec_fds();

    // Replace the process image: switch to the new table and make `sysretq`
    // resume at the new entry. Only now does the table outlive this call, so
    // disarm the guard after installing it.
    crate::mem::switch_to(table);
    task::set_pml4(table.as_u64());
    task::set_fs_base(0);
    // The new image starts with default x87/SSE registers, not the old one's.
    task::fpu::reset_live(task::current());
    task::register_bumps(table.as_u64(), started.brk, MMAP_BASE);
    crate::arch::linux::set_user_return(started.entry, started.rsp, 0x202);
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
    let result = task::process::setsid(task::current());
    if result.is_ok() {
        // A new session starts without a controlling terminal, as on Linux.
        task::linuxstate::set_ctty(None);
    }
    group_result(result)
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
    // Robust mutexes the thread still holds become owner-dead.
    super::procattr::exit_robust_list(task::current());
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
