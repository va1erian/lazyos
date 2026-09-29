//! Small syscalls that don't belong to a bigger family: `arch_prctl` (the
//! `%fs` base musl's TLS setup needs), a handful of `ioctl` requests a
//! terminal-facing program expects to succeed, `sched_getaffinity` (one CPU,
//! always), and `uname`.

use crate::task;
use crate::user_ptr;

use super::errno::{err, EINVAL, ENOTTY};

pub(super) fn sys_arch_prctl(code: u64, addr: u64) -> u64 {
    match code {
        0x1001 | 0x1002 => {
            // SET_GS / SET_FS. Only FS is used by musl.
            if code == 0x1002 {
                task::set_fs_base(addr);
            }
            0
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
