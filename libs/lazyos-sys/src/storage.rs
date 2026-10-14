//! The storage syscall (33, docs/architecture/usb-storage.md): a block
//! provider (`usbd`, for a USB stick) serves a disk to the kernel, and
//! `init` asks the kernel to mount the configured home volume once it has
//! appeared. See `kernel/src/block/provider/sys.rs` for the contract.

use crate::nr;

/// Serve a block device to the kernel (ops 0-3; the kernel also requires a
/// block-provider uid, `usbpolicy::BLOCK_PROVIDER_UIDS`).
pub use crate::cred::CAP_BLOCK_PROVIDER;

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

/// One storage op: the value, or the **positive** errno (this surface's
/// callers compare against `usbpolicy`'s errno table).
///
/// # Safety
///
/// Each pointer argument of `op` must be valid for the kernel's access.
unsafe fn storage(op: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> Result<u64, i64> {
    // SAFETY: forwarded; the caller upholds the pointer contract.
    let code = unsafe { crate::raw::syscall5(nr::STORAGE, op, a1, a2, a3, a4) };
    crate::value(code).map_err(|code| -code)
}

/// A storage op whose arguments are plain values.
fn plain(op: u64, a1: u64) -> Result<u64, i64> {
    // SAFETY: the callers pass a disk id or nothing; no pointer.
    unsafe { storage(op, a1, 0, 0, 0) }
}

/// Register a disk of `sectors` 512-byte sectors; returns its **handle**: the
/// slot (`usb<n>`, the low byte) and the disk's generation above it, which
/// the other provider ops take.
pub fn storage_register(sectors: u64, writable: bool) -> Result<u64, i64> {
    let info = [sectors, 512, u64::from(writable), 0];
    // SAFETY: the kernel reads the four words of `info`.
    unsafe { storage(op::REGISTER, info.as_ptr() as u64, 0, 0, 0) }
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
    // SAFETY: the kernel writes one four-word record into `record` and at
    // most `cap` (`data.len()`) bytes into `data`.
    let got = unsafe {
        storage(
            op::NEXT,
            id,
            record.as_mut_ptr() as u64,
            data.as_mut_ptr() as u64,
            packed,
        )
    }?;
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
    // SAFETY: the kernel only reads through `data` (a successful read's
    // bytes, which the caller passes in full); nothing is written.
    unsafe { storage(op::COMPLETE, id, tag, status, data.as_ptr() as u64) }.map(drop)
}

/// The device behind disk `id` is gone.
pub fn storage_remove(id: u64) -> Result<(), i64> {
    plain(op::REMOVE, id).map(drop)
}

/// Scan new provider disks and mount the configured home volume if it is
/// there (`init`, with `CAP_SYS_ADMIN`). Returns a [`settle_state`].
pub fn storage_settle() -> Result<u64, i64> {
    plain(op::SETTLE, 0)
}

/// Every device present when this provider started is registered: a home
/// volume still missing now is absent, and `init` stops waiting for it.
pub fn storage_scanned() -> Result<(), i64> {
    plain(op::SCANNED, 0).map(drop)
}

// A filesystem daemon's raw claim on a provider disk (ops 6-11,
// docs/architecture/usb-storage.md "Raw claims"): needs `CAP_FS_PROVIDER` and
// a filesystem-provider uid, which `mountd` and the daemons it starts have.

mod raw_op {
    pub const DISKS: u64 = 6;
    pub const CLAIM: u64 = 7;
    pub const RELEASE: u64 = 8;
    pub const READ: u64 = 9;
    pub const WRITE: u64 = 10;
    pub const FLUSH: u64 = 11;
}

/// One live disk, as [`block_disks`] lists it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockDisk {
    /// The `n` of `usb<n>`.
    pub id: u64,
    /// Counts up every time the slot gets a new disk: a claim names it.
    pub generation: u64,
    pub sectors: u64,
    pub writable: bool,
    /// Another task holds the claim.
    pub claimed: bool,
}

/// `ENODEV`: the medium is gone (a read or write of a claimed disk).
pub const ENODEV: i64 = 19;
/// `ESTALE`: the disk in that slot is another one now.
pub const ESTALE: i64 = 116;
/// `EBUSY`: someone else holds the claim.
pub const EBUSY: i64 = 16;

/// Words of one [`block_disks`] record.
const DISK_WORDS: usize = 4;

/// Every live provider disk, into `out`; how many were written.
pub fn block_disks(out: &mut [BlockDisk]) -> Result<usize, i64> {
    let mut words = [0u64; DISK_WORDS * 8];
    let room = out.len().min(words.len() / DISK_WORDS);
    let cap = (room * DISK_WORDS * 8) as u64;
    // SAFETY: the kernel writes at most `cap` bytes (`room` records) into
    // `words`.
    let count = unsafe { storage(raw_op::DISKS, words.as_mut_ptr() as u64, cap, 0, 0) }? as usize;
    for (disk, record) in out.iter_mut().zip(words.chunks(DISK_WORDS)).take(count) {
        *disk = BlockDisk {
            id: record[0],
            generation: record[1],
            sectors: record[2],
            writable: record[3] & 1 != 0,
            claimed: record[3] & 2 != 0,
        };
    }
    Ok(count.min(room))
}

/// Take the exclusive claim on disk `id`, which must still be generation
/// `generation` (`ESTALE` if the slot holds another disk, `EBUSY` if claimed).
pub fn block_claim(id: u64, generation: u64) -> Result<(), i64> {
    // SAFETY: no pointer.
    unsafe { storage(raw_op::CLAIM, id, generation, 0, 0) }.map(drop)
}

/// Give the claim on disk `id` up.
pub fn block_release(id: u64) -> Result<(), i64> {
    plain(raw_op::RELEASE, id).map(drop)
}

/// Read `buf.len()` bytes (whole sectors, at most 64 KiB) at sector `lba` of
/// the claimed disk `id`.
pub fn block_read(id: u64, lba: u64, buf: &mut [u8]) -> Result<(), i64> {
    // SAFETY: the kernel writes `buf.len()` bytes into `buf` on success.
    unsafe {
        storage(
            raw_op::READ,
            id,
            lba,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    }
    .map(drop)
}

/// Write `buf` (whole sectors, at most 64 KiB) at sector `lba` of the
/// claimed disk `id`.
pub fn block_write(id: u64, lba: u64, buf: &[u8]) -> Result<(), i64> {
    // SAFETY: the kernel only reads `buf.len()` bytes from `buf`.
    unsafe {
        storage(
            raw_op::WRITE,
            id,
            lba,
            buf.as_ptr() as u64,
            buf.len() as u64,
        )
    }
    .map(drop)
}

/// Flush the device's write cache of the claimed disk `id`.
pub fn block_flush(id: u64) -> Result<(), i64> {
    plain(raw_op::FLUSH, id).map(drop)
}
