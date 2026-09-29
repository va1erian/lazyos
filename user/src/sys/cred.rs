//! The audited credential gate (issue #101), wrapping syscall 10. The userspace
//! mirror of `kernel/src/ipc/credentials.rs`.

use core::arch::asm;

use super::SYS_CREDS;

/// Credential-gate op codes, mirroring the kernel's `process::cred_op`.
pub mod cred_op {
    /// Stamp a task with a credential block.
    pub const SET: u64 = 0;
    /// Read a task's credential block.
    pub const GET: u64 = 1;
    /// Spawn an ELF with a credential block, stamped before it can run.
    pub const SPAWN: u64 = 2;
}

/// Mirrors `kernel::ipc::credentials::CAP_SETUID`: a service holding this in
/// [`Cred::caps`] may act with root-equivalent authority for an
/// administrative check without being uid 0 itself.
pub const CAP_SETUID: u32 = 1 << 6;

/// Mirrors `kernel::ipc::credentials::CAP_SYS_TIME`: the right to step the
/// system clock.
pub const CAP_SYS_TIME: u32 = 1 << 3;

/// A task's kernel-stamped identity (issue #101), the userspace mirror of
/// `kernel/src/ipc/credentials.rs::Cred`.
///
/// Userspace can never choose this freely: [`cred_set`]/[`spawn_as`] ask the
/// kernel to validate and audit the request, and the kernel refuses a stamp
/// that would widen the caller's privilege. Login reads the user database,
/// builds one of these, and hands it to `spawn_as`.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Cred {
    /// User id; `0` is the system/root user.
    pub uid: u32,
    /// Primary group id.
    pub gid: u32,
    /// Capability bits (`CAP_*`).
    pub caps: u32,
    /// Policy label id.
    pub label_id: u32,
    /// Session id; `0` before login.
    pub session: u64,
}

impl Cred {
    /// Build a credential.
    pub const fn new(uid: u32, gid: u32, caps: u32, label_id: u32, session: u64) -> Cred {
        Cred {
            uid,
            gid,
            caps,
            label_id,
            session,
        }
    }

    /// The 40-byte wire block the native gate reads and writes.
    pub const fn to_words(self) -> [u64; 5] {
        [
            self.uid as u64,
            self.gid as u64,
            self.caps as u64,
            self.label_id as u64,
            self.session,
        ]
    }

    /// Decode the 40-byte wire block.
    pub const fn from_words(words: [u64; 5]) -> Cred {
        Cred {
            uid: words[0] as u32,
            gid: words[1] as u32,
            caps: words[2] as u32,
            label_id: words[3] as u32,
            session: words[4],
        }
    }
}

/// Invoke the native credential gate. Returns `0`/pid, or a negative errno.
fn creds_syscall(op: u64, a1: u64, a2: u64) -> i64 {
    let code: u64;
    // Safety: `int 0x80` with syscall 10; the kernel validates the credential
    // request and refuses anything the caller may not do.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_CREDS,
            in("rdi") op,
            in("rsi") a1,
            in("rdx") a2,
            lateout("rax") code,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    code as i64
}

/// Ask the kernel to stamp `target` (`None` = this task) with `cred`.
///
/// Only a service holding the set-credentials capability may do this, and the
/// kernel refuses any request that would widen the caller's own privilege; the
/// error is the negative errno (`-EPERM`, `-EACCES`, `-ESRCH`, ...).
pub fn cred_set(target: Option<u64>, cred: &Cred) -> Result<(), i64> {
    let words = cred.to_words();
    let code = creds_syscall(
        cred_op::SET,
        target.unwrap_or(u64::MAX),
        words.as_ptr() as u64,
    );
    if code == 0 {
        Ok(())
    } else {
        Err(code)
    }
}

/// Read `target`'s kernel-stamped credentials (`None` = this task) into `out`.
pub fn cred_get(target: Option<u64>, out: &mut Cred) -> Result<(), i64> {
    let mut words = [0u64; 5];
    let code = creds_syscall(
        cred_op::GET,
        target.unwrap_or(u64::MAX),
        words.as_mut_ptr() as u64,
    );
    if code == 0 {
        *out = Cred::from_words(words);
        Ok(())
    } else {
        Err(code)
    }
}

/// Spawn the program named by a **NUL-terminated** command line as a child of
/// the calling task, stamped with `cred` before it can execute one
/// instruction. Returns the child's pid, or `None` when the kernel refused the
/// request or the program could not be started.
///
/// This is the login path: `logind` authenticates a user and starts the user's
/// shell already owning that user's identity, with no window in which the
/// child could run as root.
pub fn spawn_as(cmdline_z: &[u8], cred: &Cred) -> Option<u64> {
    let words = cred.to_words();
    let code = creds_syscall(
        cred_op::SPAWN,
        cmdline_z.as_ptr() as u64,
        words.as_ptr() as u64,
    );
    (code >= 0).then_some(code as u64)
}
