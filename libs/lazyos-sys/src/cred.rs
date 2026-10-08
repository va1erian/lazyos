//! The audited credential gate (syscall 10, issue #101): the userspace
//! mirror of `kernel/src/ipc/credentials.rs`.

use crate::{nr, zero};

/// Credential-gate op codes, the kernel's `process::cred_op`.
pub mod cred_op {
    /// Stamp a task with a credential block.
    pub const SET: u64 = 0;
    /// Read a task's credential block.
    pub const GET: u64 = 1;
    // 2 and 3 were the command-line credentialed spawns, folded into
    // `spawnv` (syscall 31, fs F3); the kernel refuses them with `-EINVAL`.
    /// Read the label string for a label id.
    pub const LABEL_NAME: u64 = 4;
}

/// A `cred_get`/`cred_set` target meaning "this task".
pub const SELF_TARGET: u64 = u64::MAX;

/// Longest label the kernel accepts (`ipc::labels::MAX_LABEL_BYTES`).
pub const MAX_LABEL_BYTES: usize = 160;

/// System administration, including stopping the machine and a service.
pub const CAP_SYS_ADMIN: u32 = 1 << 2;
/// The right to step the system clock.
pub const CAP_SYS_TIME: u32 = 1 << 3;
/// Act with root-equivalent authority for an administrative check (stamp
/// credentials, read another task's) without being uid 0.
pub const CAP_SETUID: u32 = 1 << 6;
/// Read the raw input bus (`inputd`).
pub const CAP_INPUT_RAW: u32 = 1 << 9;
/// Publish onto the raw input bus as a source (an input driver).
pub const CAP_INPUT_SOURCE: u32 = 1 << 10;
/// Serve a block device to the kernel (`usbd`).
pub const CAP_BLOCK_PROVIDER: u32 = 1 << 11;
/// Serve a filesystem under `/mnt` (syscall 35).
pub const CAP_FS_PROVIDER: u32 = 1 << 12;
/// Claim the login console's keyboard (`logind`, issue #396).
pub const CAP_INPUT_CONSOLE: u32 = 1 << 13;

/// A task's kernel-stamped identity.
///
/// Userspace can never choose this freely: [`cred_set`] and `spawnv` ask the
/// kernel to validate and audit the request, and the kernel refuses a stamp
/// that would widen the caller's privilege.
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

    /// The 40-byte wire block the gate (and `spawnv`) reads and writes.
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

/// Ask the kernel to stamp `target` (`None` = this task) with `cred`. Only a
/// `CAP_SETUID` holder may, never wider than itself; the error is the
/// negative errno (`-EPERM`, `-EACCES`, `-ESRCH`, ...).
pub fn cred_set(target: Option<u64>, cred: &Cred) -> Result<(), i64> {
    let words = cred.to_words();
    // SAFETY: the kernel reads five words from `words`.
    zero(unsafe {
        crate::raw::syscall3(
            nr::CREDS,
            cred_op::SET,
            target.unwrap_or(SELF_TARGET),
            words.as_ptr() as u64,
        )
    })
}

/// Read `target`'s credentials (`None` = this task). A task may read its own;
/// another task's (a message sender's) needs `CAP_SETUID`.
pub fn cred_get(target: Option<u64>) -> Result<Cred, i64> {
    let mut words = [0u64; 5];
    // SAFETY: the kernel writes five words to `words`.
    zero(unsafe {
        crate::raw::syscall3(
            nr::CREDS,
            cred_op::GET,
            target.unwrap_or(SELF_TARGET),
            words.as_mut_ptr() as u64,
        )
    })?;
    Ok(Cred::from_words(words))
}

/// Read the label string for `label_id` into `out`, returning its length.
/// Needs `CAP_SETUID`, or `label_id` must be the caller's own label; the
/// error is the negative errno (`-EPERM`, `-ENOENT` for `0` or an unknown id).
pub fn label_name(label_id: u32, out: &mut [u8; MAX_LABEL_BYTES]) -> Result<usize, i64> {
    let mut block = [0u8; 8 + MAX_LABEL_BYTES];
    // SAFETY: the kernel writes a length word and at most `MAX_LABEL_BYTES`
    // bytes, the size of `block`.
    zero(unsafe {
        crate::raw::syscall3(
            nr::CREDS,
            cred_op::LABEL_NAME,
            label_id as u64,
            block.as_mut_ptr() as u64,
        )
    })?;
    let mut len = [0u8; 8];
    len.copy_from_slice(&block[..8]);
    let len = (u64::from_le_bytes(len) as usize).min(MAX_LABEL_BYTES);
    out[..len].copy_from_slice(&block[8..8 + len]);
    Ok(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_block_round_trips_in_kernel_order() {
        let cred = Cred::new(1000, 100, CAP_SETUID, 7, 42);
        assert_eq!(cred.to_words(), [1000, 100, 1 << 6, 7, 42]);
        assert_eq!(Cred::from_words(cred.to_words()), cred);
    }
}
