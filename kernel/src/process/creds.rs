//! Credential, quota and task-table native syscalls (creds, quota, tasks).

use super::spawn::spawn_program;
use super::*;

/// Error values the credential gate returns; the same x86_64 Linux numbering
/// the Messenger syscall uses, so userspace handling is uniform.
pub(super) const EPERM: i64 = 1;
pub(super) const ENOENT: i64 = 2;
pub(super) const ESRCH: i64 = 3;
pub(super) const ENOMEM: i64 = 12;
pub(super) const EACCES: i64 = 13;
pub(super) const EFAULT: i64 = 14;
pub(super) const EINVAL: i64 = 22;

/// The credential-gate op codes (syscall 10), mirrored by `user::sys`.
pub mod cred_op {
    /// Stamp a task with a credential block.
    pub const SET: u64 = 0;
    /// Read a task's credential block.
    pub const GET: u64 = 1;
    /// Spawn an ELF with a credential block, stamped before it can run.
    pub const SPAWN: u64 = 2;
}

/// Two's-complement `-errno` in the syscall return register.
pub(super) fn syscall_error(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

/// Map a transition refusal to its errno value.
pub(super) fn transition_error(error: TransitionError) -> u64 {
    syscall_error(match error {
        TransitionError::NotPrivileged => EPERM,
        TransitionError::Widening => EACCES,
        TransitionError::BadTarget => ESRCH,
    })
}

/// The task slot named by a `set`/`get` target: the caller for `u64::MAX`,
/// otherwise the pid.
pub(super) fn cred_target(pid: u64) -> usize {
    if pid == u64::MAX {
        task::current()
    } else {
        usize::try_from(pid).unwrap_or(usize::MAX)
    }
}

/// syscall 10: the audited credential gate (issue #101).
///
/// Every path funnels through [`credentials::transition`]/[`credentials::read`],
/// so the capability check, the no-widening rule, and the audit record live in
/// one place. See the module docs for the register ABI.
pub(super) fn sys_creds(op: u64, a1: u64, a2: u64) -> u64 {
    match op {
        cred_op::SET => {
            let Some(cred) = read_cred(a2) else {
                return syscall_error(EFAULT);
            };
            match credentials::transition(task::current(), cred_target(a1), cred) {
                Ok(_) => 0,
                Err(error) => transition_error(error),
            }
        }
        cred_op::GET => match credentials::read(task::current(), cred_target(a1)) {
            Ok(cred) => {
                if write_cred(a2, cred) {
                    0
                } else {
                    syscall_error(EFAULT)
                }
            }
            Err(error) => transition_error(error),
        },
        cred_op::SPAWN => {
            let Some(cred) = read_cred(a2) else {
                return syscall_error(EFAULT);
            };
            // Validate before a task exists, then let `spawn_program` apply the
            // same request.
            if let Err(error) = credentials::check(task::current(), cred) {
                return transition_error(error);
            }
            let code = spawn_program(a1, Some(cred));
            if code < 0 {
                syscall_error(-code)
            } else {
                code as u64
            }
        }
        _ => syscall_error(EINVAL),
    }
}

/// syscall 11: copy the calling user's quota usage and limits (issue #103).
///
/// `buf` points at [`quota::STATS_WORDS`] `u64`s: for resource `i`, word `2*i`
/// is the live usage and word `2*i + 1` the limit, in [`Resource::ALL`] order.
/// A null buffer is `-EFAULT`; limits themselves are kernel policy
/// ([`quota::set_limit`]), so this gate is read-only.
pub(super) fn sys_quota(buf: u64) -> u64 {
    if buf == 0 {
        return syscall_error(EFAULT);
    }
    let uid = credentials::of(task::current()).uid;
    let words = quota::stats_words(uid);
    if user_ptr::try_copy_words(buf, &words).is_err() {
        return syscall_error(EFAULT);
    }
    0
}

/// syscall 13: copy a [`task::introspect::TaskSnapshot`] scheduler snapshot
/// into the caller's buffer (MCP debug bridge Phase 2, `docs/mcp-debug-bridge.md`).
///
/// `buf` points at [`task::introspect::WORDS`] `u64`s. A null buffer is
/// `-EFAULT`; like [`sys_quota`], this gate is read-only and discloses no
/// more than `messengerctl sessions` already does.
pub(super) fn sys_tasks(buf: u64) -> u64 {
    if buf == 0 {
        return syscall_error(EFAULT);
    }
    let words = task::introspect::snapshot_words();
    let mut bytes = Vec::with_capacity(words.len() * 8);
    for word in words.iter() {
        bytes.extend_from_slice(&word.to_ne_bytes());
    }
    // Validate the whole destination as writable user memory first; this
    // gate is open to every task, so a raw write would be a kernel-write
    // primitive.
    match crate::ipc::syscalls::copy_out(buf, &bytes) {
        Ok(()) => 0,
        Err(code) => (code as u64).wrapping_neg(),
    }
}
