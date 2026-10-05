//! The user-space filesystem syscall (35, docs/smb-plan.md F1): a daemon
//! mounts at `/mnt/<name>` and serves the VFS's requests. The records and
//! payloads are `libs/fused`; [`Mount`] is the [`fused::daemon::Provider`]
//! a daemon hands to [`fused::daemon::serve_one`].

use core::arch::asm;

use fused::daemon::Provider;
use fused::wire::{sys_op, Reply, Request, REPLY_WORDS, REQUEST_WORDS, SYS_FUSE};

/// Serve a filesystem under `/mnt` (syscall 35).
pub const CAP_FS_PROVIDER: u32 = 1 << 12;

/// Ticks one `NEXT` waits before returning empty-handed (the kernel caps it
/// at one second).
const NEXT_WAIT_TICKS: u64 = 100;

fn fuse(op: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> Result<u64, i64> {
    let code: u64;
    // SAFETY: `int 0x80` with syscall 35; the kernel validates every pointer
    // argument against this task's address space before using it.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_FUSE,
            in("rdi") op,
            in("rsi") a1,
            in("rdx") a2,
            in("r10") a3,
            in("r8") a4,
            lateout("rax") code,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    let code = code as i64;
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
        let id = fuse(
            sys_op::REGISTER,
            name.as_ptr() as u64,
            name.len() as u64,
            flags,
            0,
        )?;
        Ok(Mount { id })
    }
}

impl Provider for Mount {
    fn next(&mut self, payload: &mut [u8]) -> Result<Option<Request>, i64> {
        let mut record = [0u64; REQUEST_WORDS];
        let cap = payload.len().min(u32::MAX as usize) as u64;
        let deadline = super::clock() + NEXT_WAIT_TICKS;
        let packed = cap | deadline.min(u64::from(u32::MAX)) << 32;
        let got = fuse(
            sys_op::NEXT,
            self.id,
            record.as_mut_ptr() as u64,
            payload.as_mut_ptr() as u64,
            packed,
        )?;
        Ok((got == 1).then(|| Request::from_words(&record)))
    }

    fn reply(&mut self, reply: &Reply, data: &[u8]) -> Result<(), i64> {
        let record: [u64; REPLY_WORDS] = reply.to_words();
        fuse(
            sys_op::REPLY,
            self.id,
            record.as_ptr() as u64,
            data.as_ptr() as u64,
            0,
        )
        .map(|_| ())
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        let _ = fuse(sys_op::UNREGISTER, self.id, 0, 0, 0);
    }
}
