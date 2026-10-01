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
    /// Spawn an ELF stamped with a label string (`CAP_SETUID`).
    pub const SPAWN_LABELLED: u64 = 3;
    /// Read the label string for a label id.
    pub const LABEL_NAME: u64 = 4;
}

/// Longest label the kernel accepts (`ipc::labels::MAX_LABEL_BYTES`).
pub const MAX_LABEL_BYTES: usize = 160;

/// Mirrors `kernel::ipc::credentials::CAP_SETUID`: a service holding this in
/// [`Cred::caps`] may act with root-equivalent authority for an
/// administrative check without being uid 0 itself.
pub const CAP_SETUID: u32 = 1 << 6;

/// Mirrors `kernel::ipc::credentials::CAP_SYS_TIME`: the right to step the
/// system clock.
pub const CAP_SYS_TIME: u32 = 1 << 3;

/// Mirrors `kernel::ipc::credentials::CAP_SYS_ADMIN`: system administration,
/// including stopping the machine (`power`) and a service (the lifecycle
/// `Shutdown` message).
pub const CAP_SYS_ADMIN: u32 = 1 << 2;

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

/// Like [`spawn_as`], but the child is stamped with the label string `label`
/// (`app:<reverse.dns.name>` or `system:<name>`) as well: the kernel interns
/// it and records the id in the child's credentials. The `label_id` inside
/// `cred` is ignored. Only a `CAP_SETUID` holder that is unlabelled (or already
/// in that label) may do this, and a label can never change afterwards.
pub fn spawn_as_labelled(cmdline_z: &[u8], cred: &Cred, label: &str) -> Option<u64> {
    let cred = cred.to_words();
    let block = [
        cred[0],
        cred[1],
        cred[2],
        0,
        cred[4],
        label.as_ptr() as u64,
        label.len() as u64,
    ];
    let code = creds_syscall(
        cred_op::SPAWN_LABELLED,
        cmdline_z.as_ptr() as u64,
        block.as_ptr() as u64,
    );
    (code >= 0).then_some(code as u64)
}

/// Read the label string for `label_id` into `out`, returning its length.
/// Needs `CAP_SETUID`, or `label_id` must be the caller's own label; the
/// error is the negative errno (`-EPERM`, `-ENOENT` for `0` or an unknown id).
pub fn label_name(label_id: u32, out: &mut [u8; MAX_LABEL_BYTES]) -> Result<usize, i64> {
    let mut block = [0u8; 8 + MAX_LABEL_BYTES];
    let code = creds_syscall(
        cred_op::LABEL_NAME,
        label_id as u64,
        block.as_mut_ptr() as u64,
    );
    if code != 0 {
        return Err(code);
    }
    let len =
        (u64::from_le_bytes(block[..8].try_into().unwrap_or([0; 8])) as usize).min(MAX_LABEL_BYTES);
    out[..len].copy_from_slice(&block[8..8 + len]);
    Ok(len)
}
