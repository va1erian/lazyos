//! The user-space filesystem syscall (35, docs/smb-plan.md F1): a daemon
//! mounts at `/mnt/<name>` and serves the VFS's requests. The records and
//! payloads are `libs/fused`; [`Mount`] is the [`fused::daemon::Provider`]
//! a daemon hands to [`fused::daemon::serve_one`].

use fused::daemon::Provider;
use fused::wire::{sys_op, Reply, Request, REPLY_WORDS, REQUEST_WORDS};
use lazyos_sys::nr;

pub use lazyos_sys::cred::CAP_FS_PROVIDER;

/// Ticks one `NEXT` waits before returning empty-handed (the kernel caps it
/// at one second).
const NEXT_WAIT_TICKS: u64 = 100;

/// One provider op: the value, or the positive errno.
///
/// # Safety
///
/// Each pointer argument of `op` must be valid for the kernel's access.
unsafe fn fuse(op: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> Result<u64, i64> {
    // SAFETY: forwarded; the caller upholds the pointer contract.
    let code = unsafe { lazyos_sys::raw::syscall5(nr::FUSE, op, a1, a2, a3, a4) };
    if code < 0 {
        Err(-code)
    } else {
        Ok(code as u64)
    }
}

/// A mounted provider: unmounted when dropped (and by the kernel when the
/// task dies).
pub struct Mount {
    id: u64,
}

impl Mount {
    /// Mount a new provider at `/mnt/<name>` with `flags`
    /// (`fused::wire::FLAG_RO`, `FLAG_NOEXEC`). The error is an errno.
    pub fn register(name: &str, flags: u64) -> Result<Mount, i64> {
        // SAFETY: the kernel reads `name.len()` bytes of `name`.
        let id = unsafe {
            fuse(
                sys_op::REGISTER,
                name.as_ptr() as u64,
                name.len() as u64,
                flags,
                0,
            )
        }?;
        Ok(Mount { id })
    }
}

impl Provider for Mount {
    fn next(&mut self, payload: &mut [u8]) -> Result<Option<Request>, i64> {
        let mut record = [0u64; REQUEST_WORDS];
        let cap = payload.len().min(u32::MAX as usize) as u64;
        let deadline = super::clock() + NEXT_WAIT_TICKS;
        let packed = cap | deadline.min(u64::from(u32::MAX)) << 32;
        // SAFETY: the kernel writes one request record into `record` and at
        // most `cap` (`payload.len()`) bytes into `payload`.
        let got = unsafe {
            fuse(
                sys_op::NEXT,
                self.id,
                record.as_mut_ptr() as u64,
                payload.as_mut_ptr() as u64,
                packed,
            )
        }?;
        Ok((got == 1).then(|| Request::from_words(&record)))
    }

    fn reply(&mut self, reply: &Reply, data: &[u8]) -> Result<(), i64> {
        let record: [u64; REPLY_WORDS] = reply.to_words();
        // SAFETY: the kernel reads the reply record and the payload length
        // it names from `data`; nothing is written.
        unsafe {
            fuse(
                sys_op::REPLY,
                self.id,
                record.as_ptr() as u64,
                data.as_ptr() as u64,
                0,
            )
        }
        .map(|_| ())
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        // SAFETY: no pointer crosses the gate.
        let _ = unsafe { fuse(sys_op::UNREGISTER, self.id, 0, 0, 0) };
    }
}
