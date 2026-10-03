//! The storage syscall (33, docs/architecture/usb-storage.md): a block
//! provider (`usbd`, for a USB stick) serves a disk to the kernel, and
//! `init` asks the kernel to mount the configured home volume once it has
//! appeared. See `kernel/src/block/provider/sys.rs` for the contract.

use core::arch::asm;

/// Serve a block device to the kernel (ops 0-3; the kernel also requires a
/// block-provider uid, `usbpolicy::BLOCK_PROVIDER_UIDS`).
pub const CAP_BLOCK_PROVIDER: u32 = 1 << 11;

/// `storage(op, a1, a2, a3, a4)`.
pub const SYS_STORAGE: u64 = 33;

mod op {
    pub const REGISTER: u64 = 0;
    pub const NEXT: u64 = 1;
    pub const COMPLETE: u64 = 2;
    pub const REMOVE: u64 = 3;
    pub const SETTLE: u64 = 4;
    pub const SCANNED: u64 = 5;
}

/// Request operations, as [`StorageRequest::op`] carries them.
pub mod storage_op {
    pub const READ: u64 = 1;
    pub const WRITE: u64 = 2;
    pub const FLUSH: u64 = 3;
}

/// Completion statuses for [`storage_complete`].
pub mod storage_status {
    pub const OK: u64 = 0;
    pub const IO: u64 = 1;
    pub const READ_ONLY: u64 = 2;
    /// The medium is gone: the kernel's disk dies with this request.
    pub const GONE: u64 = 3;
}

/// What [`storage_settle`] reports.
pub mod settle_state {
    pub const NONE: u64 = 0;
    pub const MOUNTED: u64 = 1;
    pub const WAITING: u64 = 2;
    pub const MOUNTED_EARLIER: u64 = 3;
    /// Not there, and every provider finished scanning what was present.
    pub const ABSENT: u64 = 4;
}

/// One request the kernel handed out ([`storage_next`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StorageRequest {
    pub tag: u64,
    pub op: u64,
    /// First 512-byte sector.
    pub lba: u64,
    /// Bytes to move (0 for a flush).
    pub bytes: u64,
}

fn storage(op: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> Result<u64, i64> {
    let code: u64;
    // SAFETY: `int 0x80` with syscall 33; the kernel validates every pointer
    // argument against this task's address space before using it.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_STORAGE,
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

/// Register a disk of `sectors` 512-byte sectors; returns its id (`usb<id>`).
pub fn storage_register(sectors: u64, writable: bool) -> Result<u64, i64> {
    let info = [sectors, 512, u64::from(writable), 0];
    storage(op::REGISTER, info.as_ptr() as u64, 0, 0, 0)
}

/// Wait until `deadline` (absolute ticks, at most one second ahead) for the
/// next request of disk `id`. A write's data lands in `data`.
pub fn storage_next(
    id: u64,
    data: &mut [u8],
    deadline: u64,
) -> Result<Option<StorageRequest>, i64> {
    let mut record = [0u64; 4];
    let cap = data.len().min(u32::MAX as usize) as u64;
    let packed = cap | deadline.min(u64::from(u32::MAX)) << 32;
    let got = storage(
        op::NEXT,
        id,
        record.as_mut_ptr() as u64,
        data.as_mut_ptr() as u64,
        packed,
    )?;
    Ok((got == 1).then_some(StorageRequest {
        tag: record[0],
        op: record[1],
        lba: record[2],
        bytes: record[3],
    }))
}

/// Finish request `tag` with `status`; a successful read passes exactly the
/// request's bytes in `data`.
pub fn storage_complete(id: u64, tag: u64, status: u64, data: &[u8]) -> Result<(), i64> {
    storage(op::COMPLETE, id, tag, status, data.as_ptr() as u64).map(|_| ())
}

/// The device behind disk `id` is gone.
pub fn storage_remove(id: u64) -> Result<(), i64> {
    storage(op::REMOVE, id, 0, 0, 0).map(|_| ())
}

/// Scan new provider disks and mount the configured home volume if it is
/// there (`init`, with `CAP_SYS_ADMIN`). Returns a [`settle_state`].
pub fn storage_settle() -> Result<u64, i64> {
    storage(op::SETTLE, 0, 0, 0, 0)
}

/// Every device present when this provider started is registered: a home
/// volume still missing now is absent, and `init` stops waiting for it.
pub fn storage_scanned() -> Result<(), i64> {
    storage(op::SCANNED, 0, 0, 0, 0).map(|_| ())
}
