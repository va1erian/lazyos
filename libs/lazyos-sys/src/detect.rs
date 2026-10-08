//! Whether this process runs on LazyOS, so a portable program issues
//! `int 0x80` only there: on another kernel that instruction is a different
//! ABI, or a crash.

/// Linux `uname(2)`; LazyOS answers it with sysname `LazyOS`.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const SYS_UNAME: u64 = 63;
/// `struct utsname`: six 65-byte fields, sysname first.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const UTS_FIELD: usize = 65;

/// True on LazyOS. A Linux-ABI (musl) program asks the kernel's name through
/// the Linux `uname` syscall, valid on both kernels; a native program
/// (`target_os = "none"`) is on LazyOS by construction; anything else (a
/// Windows or macOS host build) never is.
pub fn on_lazyos() -> bool {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return sysname_is(b"LazyOS");
    #[cfg(target_os = "none")]
    return true;
    #[cfg(not(any(all(target_os = "linux", target_arch = "x86_64"), target_os = "none")))]
    return false;
}

/// Whether the running kernel's `sysname` is `name`.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn sysname_is(name: &[u8]) -> bool {
    let mut uts = [0u8; UTS_FIELD * 6];
    let code: i64;
    // SAFETY: Linux `uname` (63) via `syscall`: rdi points at a writable
    // `utsname`-sized buffer on this frame; rcx/r11 are clobbered by the
    // instruction. LazyOS and Linux both implement it.
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") SYS_UNAME as i64 => code,
            in("rdi") uts.as_mut_ptr(),
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    let end = uts[..UTS_FIELD]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(UTS_FIELD);
    code == 0 && &uts[..end] == name
}
