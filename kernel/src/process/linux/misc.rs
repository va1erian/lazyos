//! Small syscalls that don't belong to a bigger family: `arch_prctl` (the
//! `%fs` base musl's TLS setup needs), a handful of `ioctl` requests a
//! terminal-facing program expects to succeed, `sched_getaffinity` (one CPU,
//! always), and `uname`.

use crate::task;
use crate::user_ptr;

use super::errno::{err, EBADF, EFAULT, EINVAL, ENOTTY, EPERM};

pub(super) fn sys_arch_prctl(code: u64, addr: u64) -> u64 {
    match code {
        0x1001 => 0, // SET_GS: not used by musl; the kernel programs no GS base.
        0x1002 => {
            // SET_FS. Reject a non-canonical base before it reaches the MSR:
            // `wrmsr IA32_FS_BASE` would raise #GP in ring 0 (issue #222).
            if task::set_fs_base(addr) {
                0
            } else {
                err(EINVAL)
            }
        }
        0x1003 | 0x1004 => {
            // GET_FS / GET_GS: write the base to *addr.
            // Safety: user pointer (the syscall ABI's contract).
            unsafe { user_ptr::write::<u64>(addr, 0) };
            0
        }
        _ => err(EINVAL),
    }
}

pub(super) fn sys_ioctl(fd: u64, request: u64, arg: u64) -> u64 {
    match request {
        0x5401 => 0, // TCGETS: report a default (zeroed) termios
        0x540F => {
            // TIOCGPGRP: report the foreground process group. There is no
            // separate controlling-terminal group yet, so it is the caller's
            // own group (which `getpgrp` reports too).
            // Safety: user `pid_t *` (the syscall ABI's contract).
            unsafe { user_ptr::write::<u32>(arg, task::pgid() as u32) };
            0
        }
        0x5410 => 0, // TIOCSPGRP
        0x5421 => {
            // FIONBIO: the nonblocking switch std::net uses on sockets and pipes
            // (`set_nonblocking`), the ioctl twin of `fcntl(F_SETFL, O_NONBLOCK)`.
            let Ok(on) = user_ptr::try_read::<i32>(arg) else {
                return err(EFAULT);
            };
            if task::fd_set_status(fd as usize, on != 0) {
                0
            } else {
                err(EBADF)
            }
        }
        0x5451 | 0x5450 => {
            // FIOCLEX / FIONCLEX: set or clear close-on-exec.
            if task::fd_set_cloexec(fd as usize, request == 0x5451) {
                0
            } else {
                err(EBADF)
            }
        }
        0x5413 => {
            // TIOCGWINSZ: 24 rows x 80 columns.
            // Safety: user `struct winsize` (the syscall ABI's contract).
            unsafe {
                user_ptr::write::<u16>(arg, 24);
                user_ptr::write::<u16>(arg + 2, 80);
                user_ptr::write::<u16>(arg + 4, 0);
                user_ptr::write::<u16>(arg + 6, 0);
            }
            0
        }
        _ if fd <= 2 => 0,
        _ => err(ENOTTY),
    }
}

pub(super) fn sys_sched_getaffinity(mask: u64, len: u64) -> u64 {
    if len >= 8 {
        // Safety: user buffer (the syscall ABI's contract).
        unsafe { user_ptr::write::<u64>(mask, 1) };
        8
    } else if len > 0 {
        // Safety: user buffer (the syscall ABI's contract).
        unsafe { user_ptr::write::<u8>(mask, 1) };
        1
    } else {
        0
    }
}

pub(super) fn sys_uname(buf: u64) -> u64 {
    // struct utsname: six 65-byte fields.
    let mut data = [0u8; 6 * 65];
    let fields = ["LazyOS", "lazyos", "0.1.0", "0.1.0", "x86_64", "unknown"];
    for (i, field) in fields.iter().enumerate() {
        let bytes = field.as_bytes();
        data[i * 65..i * 65 + bytes.len()].copy_from_slice(bytes);
    }
    // Safety: user buffer of at least 390 bytes (musl's utsname, the
    // syscall ABI's contract).
    unsafe { user_ptr::copy_to(buf, &data) };
    0
}

/// `syslog(action, buf, len)` (`klogctl`): the kernel log ring (`klog`), so
/// BusyBox `dmesg` shows the boot log on a machine with no serial port.
/// Reading is open to every task, as on a Linux with `dmesg_restrict=0`: the
/// ring holds boot and driver messages, never user data. Clearing it is
/// refused (`EPERM`), so nobody can erase the evidence of a boot.
pub(super) fn sys_syslog(action: u64, buf: u64, len: u64) -> u64 {
    const CLOSE: u64 = 0;
    const OPEN: u64 = 1;
    const READ_ALL: u64 = 3;
    const SIZE_UNREAD: u64 = 9;
    const SIZE_BUFFER: u64 = 10;
    match action {
        CLOSE | OPEN => 0,
        SIZE_BUFFER => crate::klog::CAPACITY as u64,
        SIZE_UNREAD => 0,
        READ_ALL => {
            if buf == 0 {
                return err(EINVAL);
            }
            let want = usize::try_from(len)
                .unwrap_or(usize::MAX)
                .min(crate::klog::CAPACITY);
            let mut data = alloc::vec![0u8; want];
            let count = crate::klog::snapshot(&mut data);
            if user_ptr::try_copy_to(buf, &data[..count]).is_err() {
                return err(EFAULT);
            }
            count as u64
        }
        // Clear, console level/enable/disable: privileged on Linux too.
        4..=8 => err(EPERM),
        _ => err(EINVAL),
    }
}
