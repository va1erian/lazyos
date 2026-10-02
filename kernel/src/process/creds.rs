//! Credential, quota and task-table native syscalls (creds, quota, tasks).

use super::*;
use crate::ipc::credentials::LabelStamp;
use crate::ipc::labels;

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
    // 2 and 3 were the command-line credentialed spawns (`SPAWN`,
    // `SPAWN_LABELLED`). `spawnv` (syscall 30) took them over with the same
    // checks; the gate now answers them with `-EINVAL` like any unknown op.
    /// Copy the label string for id `a1` into `a2`, a [`LABEL_BUF_BYTES`]
    /// buffer: one length word, then the bytes.
    pub const LABEL_NAME: u64 = 4;
}

/// Size of the buffer [`cred_op::LABEL_NAME`] fills: an 8-byte length word and
/// room for the longest label.
pub const LABEL_BUF_BYTES: usize = 8 + crate::ipc::labels::MAX_LABEL_BYTES;

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
        TransitionError::LabelLocked => EPERM,
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
        cred_op::LABEL_NAME => label_name(a1, a2),
        _ => syscall_error(EINVAL),
    }
}

/// Approve a labelled spawn for the calling task: check the privilege, intern
/// `label` and check the assigned stamp. Returns the credential the child is
/// stamped with (its `label_id` the interned id), or the syscall error value.
/// Interning is the only side effect of a refusal, and it is bounded by the
/// table capacity. Used by `spawnv`'s `AsLabelled` mode.
pub(super) fn approve_labelled(mut cred: Cred, label: &str) -> Result<Cred, u64> {
    let actor = task::current();
    // Privilege first: an unprivileged caller must not learn anything about
    // the label table (full, duplicate) through the error it gets back.
    credentials::check_stamp(
        actor,
        LabelStamp::Keep { current: 0 },
        Cred {
            label_id: 0,
            ..cred
        },
    )
    .map_err(transition_error)?;
    let id = labels::intern(label).map_err(|_| syscall_error(EINVAL))?;
    cred.label_id = id;
    credentials::check_stamp(actor, LabelStamp::Assign, cred).map_err(transition_error)?;
    Ok(cred)
}

/// [`cred_op::LABEL_NAME`]: copy a label string out to the caller.
fn label_name(id: u64, buf: u64) -> u64 {
    let Ok(id) = u32::try_from(id) else {
        return syscall_error(EINVAL);
    };
    let name = match labels::read_name(task::current(), id) {
        Ok(Some(name)) => name,
        Ok(None) => return syscall_error(ENOENT),
        Err(error) => return transition_error(error),
    };
    let mut block = [0u8; LABEL_BUF_BYTES];
    block[..8].copy_from_slice(&(name.len() as u64).to_le_bytes());
    block[8..8 + name.len()].copy_from_slice(name.as_bytes());
    if buf != 0 && user_ptr::try_copy_to(buf, &block).is_ok() {
        0
    } else {
        syscall_error(EFAULT)
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
