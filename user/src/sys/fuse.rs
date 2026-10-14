//! The user-space filesystem syscall (35, docs/smb-plan.md F1): a daemon
//! mounts at `/mnt/<name>` and serves the VFS's requests. The records and
//! payloads are `libs/fused`; [`Mount`] is the [`fused::daemon::Provider`]
//! a daemon hands to [`fused::daemon::serve_one`].

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use fused::daemon::Provider;
use fused::wire::{
    sys_op, ProviderInfo, Reply, Request, PROVIDER_WORDS, REPLY_WORDS, REQUEST_WORDS, STOP_PENDING,
};
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
    /// A supervisor asked this daemon to finish ([`stop`]); learned from the
    /// `NEXT` that found nothing queued.
    stopping: bool,
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
        Ok(Mount {
            id,
            stopping: false,
        })
    }

    /// Whether the supervisor asked this daemon to finish (a [`stop`] of its
    /// mount): the last `serve_one` found nothing to answer and the kernel
    /// will refuse every new request. Flush the volume, then drop the
    /// [`Mount`] (which unregisters it) and exit.
    pub fn stop_requested(&self) -> bool {
        self.stopping
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
        if got == STOP_PENDING {
            self.stopping = true;
        }
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

/// Most removable-media providers one [`providers`] call reports (the
/// kernel serves eight).
const MAX_LISTED: usize = 8;
/// Bytes of the report [`holders`] asks for.
const HOLDERS_ROOM: usize = 1024;

/// Every live removable-media provider. A supervisor's call (`mountd`:
/// `CAP_FS_PROVIDER` and a provider uid, like every op).
pub fn providers() -> Result<Vec<ProviderInfo>, i64> {
    let mut words = vec![0u64; MAX_LISTED * PROVIDER_WORDS];
    // SAFETY: the kernel writes at most `cap` bytes of whole records into
    // `words`, which holds `cap` bytes.
    let count = unsafe {
        fuse(
            sys_op::LIST,
            words.as_mut_ptr() as u64,
            (words.len() * 8) as u64,
            0,
            0,
        )
    }?;
    Ok(words
        .as_chunks::<PROVIDER_WORDS>()
        .0
        .iter()
        .take(count as usize)
        .filter_map(ProviderInfo::from_words)
        .collect())
}

/// Ask provider `id`, registered by task `owner` (the pid [`providers`]
/// reported), to finish: flush, unmount and exit on its own. Unless `force`,
/// `Err(EBUSY)` (errno 16) while a file is open on it; ask [`holders`] who.
/// `Err(ESRCH)` (3) for a provider that is not there. A supervisor's call
/// (`mountd`, or root).
pub fn stop(id: u64, owner: u64, force: bool) -> Result<(), i64> {
    // SAFETY: no pointer crosses the gate.
    unsafe { fuse(sys_op::STOP, id, u64::from(force), owner, 0) }.map(|_| ())
}

/// Who keeps provider `id` (registered by `owner`) busy, one line per task
/// and file (empty when nobody does).
pub fn holders(id: u64, owner: u64) -> Result<String, i64> {
    let mut text = vec![0u8; HOLDERS_ROOM];
    // SAFETY: the kernel writes at most `text.len()` bytes into `text`.
    let len = unsafe {
        fuse(
            sys_op::HOLDERS,
            id,
            text.as_mut_ptr() as u64,
            text.len() as u64,
            owner,
        )
    }?;
    text.truncate(len as usize);
    Ok(String::from_utf8_lossy(&text).into_owned())
}
