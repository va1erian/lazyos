//! The `int 0x80` gate itself: the only place userspace issues it.
//!
//! Register convention: `rax` is the syscall number, the arguments go in
//! `rdi`, `rsi`, `rdx`, `r10`, `r8`, and the result comes back in `rax` (a
//! value, or a negative errno). The kernel's stub saves and restores the
//! argument registers, but `rcx` and `r11` are *not* preserved, so the asm
//! declares `clobber_abi("sysv64")`; without it the compiler may keep a live
//! value in them across the gate (issue #91 hit exactly that in `bootstrap()`).
//!
//! Every function here is `unsafe`: an argument may be a pointer the kernel
//! reads or writes, and only the typed wrappers know which. The kernel
//! validates each pointer against the caller's address space, so a bad one
//! fails with `-EFAULT`, but a *valid* one the kernel writes through must not
//! alias anything Rust holds a reference to.
//!
//! On anything but x86-64 (a host build of a portable crate) there is no
//! gate: every call returns `-ENOSYS` and [`exit`] spins.

/// One native syscall with five arguments; the raw `rax` value.
///
/// # Safety
///
/// Every argument the syscall `nr` treats as a pointer must be valid for the
/// access the kernel makes through it (its documented length, read or
/// write), for the duration of the call, and a buffer the kernel writes must
/// not be aliased by a live shared reference.
#[inline]
pub unsafe fn syscall5(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i64 {
    #[cfg(target_arch = "x86_64")]
    {
        let code: u64;
        // SAFETY: the native gate with its register convention (see the
        // module docs); the caller upholds the pointer contract above, and
        // rcx/r11 plus every sysv64 caller-saved register are declared
        // clobbered.
        unsafe {
            core::arch::asm!(
                "int 0x80",
                inlateout("rax") nr => code,
                in("rdi") a1,
                in("rsi") a2,
                in("rdx") a3,
                in("r10") a4,
                in("r8") a5,
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack),
                clobber_abi("sysv64"),
            );
        }
        code as i64
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (nr, a1, a2, a3, a4, a5);
        -crate::errno::ENOSYS
    }
}

/// [`syscall5`] with three arguments (`r10` and `r8` are zero).
///
/// # Safety
///
/// As [`syscall5`].
#[inline]
pub unsafe fn syscall3(nr: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    // SAFETY: forwarded; the caller upholds `syscall5`'s contract.
    unsafe { syscall5(nr, a1, a2, a3, 0, 0) }
}

/// [`syscall5`] with two arguments.
///
/// # Safety
///
/// As [`syscall5`].
#[inline]
pub unsafe fn syscall2(nr: u64, a1: u64, a2: u64) -> i64 {
    // SAFETY: forwarded; the caller upholds `syscall5`'s contract.
    unsafe { syscall5(nr, a1, a2, 0, 0, 0) }
}

/// [`syscall5`] with one argument.
///
/// # Safety
///
/// As [`syscall5`].
#[inline]
pub unsafe fn syscall1(nr: u64, a1: u64) -> i64 {
    // SAFETY: forwarded; the caller upholds `syscall5`'s contract.
    unsafe { syscall5(nr, a1, 0, 0, 0, 0) }
}

/// A syscall with no arguments and no pointers: always safe to issue.
#[inline]
pub fn syscall0(nr: u64) -> i64 {
    // SAFETY: no argument reaches the kernel, so no pointer crosses the gate.
    unsafe { syscall5(nr, 0, 0, 0, 0, 0) }
}

/// Terminate the calling task with `code` (syscall 0); never returns.
pub fn exit(code: u32) -> ! {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: syscall 0 takes no pointer and does not return to this task.
    unsafe {
        core::arch::asm!(
            "int 0x80",
            in("rax") crate::nr::EXIT,
            in("rdi") code as u64,
            options(noreturn, nostack),
        );
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = code;
        loop {
            core::hint::spin_loop();
        }
    }
}
