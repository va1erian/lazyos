//! Syscall 33, ops 6-11: a filesystem daemon's raw access to a provider
//! disk (docs/architecture/usb-storage.md "Raw claims").
//!
//! ```text
//!   op 6 DISKS(buf, cap)         the live disks: four words each
//!                                {id, generation, sectors, flags (bit 0
//!                                writable, bit 1 claimed)}; at most
//!                                `cap` bytes of them -> how many
//!   op 7 CLAIM(id, generation)   the exclusive claim on the disk, which
//!                                must be that generation -> 0
//!   op 8 RELEASE(id)             give the claim up -> 0
//!   op 9 READ(id, lba, buf, n)   n bytes (whole sectors, at most 64 KiB)
//!                                from `lba` into `buf` -> n
//!   op 10 WRITE(id, lba, buf, n) the same, to the disk -> n
//!   op 11 FLUSH(id)              the device's write cache -> 0
//! ```
//!
//! The caller needs `CAP_FS_PROVIDER`, a filesystem-provider uid
//! (`mounttable::FS_PROVIDER_UIDS`) **and** to have been started from
//! `fatfuse`'s program ([`may_claim`]). A claim is exclusive, refused on a
//! disk the kernel mounted a volume through, and tied to the generation of
//! the disk it was taken on; reads and writes go through the same requests the
//! kernel's own filesystems make, data only ever through a kernel buffer.
//! A medium that is gone answers `ENODEV`, which the daemon takes as the end
//! of its volume.

use alloc::vec::Vec;

use super::{claim, list, raw_flush, raw_read, raw_write, release, RawError, MAX_REQUEST_BYTES};
use crate::ipc::credentials::{self, CAP_FS_PROVIDER};
use crate::{task, user_ptr};

pub mod op {
    pub const DISKS: u64 = 6;
    pub const CLAIM: u64 = 7;
    pub const RELEASE: u64 = 8;
    pub const READ: u64 = 9;
    pub const WRITE: u64 = 10;
    pub const FLUSH: u64 = 11;
}

/// Words of one [`op::DISKS`] record.
pub const DISK_WORDS: usize = 4;
/// [`op::DISKS`] flag bits.
pub const FLAG_WRITABLE: u64 = 1;
pub const FLAG_CLAIMED: u64 = 2;

const EPERM: i64 = 1;
const ENOENT: i64 = 2;
const EIO: i64 = 5;
const EFAULT: i64 = 14;
const EBUSY: i64 = 16;
const ENODEV: i64 = 19;
const EINVAL: i64 = 22;
const EROFS: i64 = 30;
const ESTALE: i64 = 116;

fn negative(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

fn errno(error: RawError) -> u64 {
    negative(match error {
        RawError::NoDisk => ENOENT,
        RawError::Stale => ESTALE,
        RawError::Busy => EBUSY,
        RawError::NotOwner => EPERM,
        RawError::Gone => ENODEV,
        RawError::Io => EIO,
        RawError::ReadOnly => EROFS,
        RawError::Range => EINVAL,
    })
}

/// Whether task `me` may claim and use a disk raw: a filesystem provider
/// (`CAP_FS_PROVIDER` and a provider uid) **started from the volume daemon's
/// program**. `ftpfuse` and `smbfuse` hold the same capability and uid (they
/// are `mountd`'s children too) and parse hostile network input; the program
/// path is what keeps them from the disks. It is the path `spawnv` was given
/// (`task::linuxstate::record_program`), and `/system/bin` is no one's to
/// write, so no other program can pass for it.
pub fn may_claim(me: usize) -> bool {
    may_list(me) && task::linuxstate::program_of(me).is_some_and(|path| &*path == fhs::bin::FATFUSE)
}

/// Whether task `me` may list the disks (which `mountd`, deciding what to
/// start a daemon for, does): a filesystem provider, started from any program.
pub fn may_list(me: usize) -> bool {
    let cred = credentials::of(me);
    me != task::KERNEL_TASK
        && cred.has_cap(CAP_FS_PROVIDER)
        && mounttable::FS_PROVIDER_UIDS.contains(&cred.uid)
}

/// Ops 6-11 for task `me`.
pub fn dispatch(me: usize, operation: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> u64 {
    let allowed = if operation == op::DISKS {
        may_list(me)
    } else {
        may_claim(me)
    };
    if !allowed {
        return negative(EPERM);
    }
    let Ok(id) = usize::try_from(a1) else {
        // Only DISKS takes something else first (a pointer).
        return if operation == op::DISKS {
            disks(a1, a2)
        } else {
            negative(EINVAL)
        };
    };
    match operation {
        op::DISKS => disks(a1, a2),
        op::CLAIM => match claim(id, a2, me) {
            Ok(()) => 0,
            Err(error) => errno(error),
        },
        op::RELEASE => match release(id, me) {
            Ok(()) => 0,
            Err(error) => errno(error),
        },
        op::READ => read(me, id, a2, a3, a4),
        op::WRITE => write(me, id, a2, a3, a4),
        op::FLUSH => match raw_flush(id, me) {
            Ok(()) => 0,
            Err(error) => errno(error),
        },
        _ => negative(EINVAL),
    }
}

fn disks(buf: u64, cap: u64) -> u64 {
    let room = usize::try_from(cap).unwrap_or(usize::MAX) / (DISK_WORDS * 8);
    let mut words: Vec<u64> = Vec::new();
    let mut count = 0u64;
    for disk in list().into_iter().take(room) {
        let mut flags = 0;
        if disk.writable {
            flags |= FLAG_WRITABLE;
        }
        if disk.claimed {
            flags |= FLAG_CLAIMED;
        }
        words.extend_from_slice(&[disk.id as u64, disk.generation, disk.sectors, flags]);
        count += 1;
    }
    if count == 0 {
        return 0;
    }
    match user_ptr::try_copy_words(buf, &words) {
        Ok(()) => count,
        Err(_) => negative(EFAULT),
    }
}

fn length(bytes: u64) -> Option<usize> {
    usize::try_from(bytes)
        .ok()
        .filter(|&n| n <= MAX_REQUEST_BYTES)
}

fn read(me: usize, id: usize, lba: u64, buf: u64, bytes: u64) -> u64 {
    let Some(len) = length(bytes) else {
        return negative(EINVAL);
    };
    // The destination must be writable before the device is asked.
    if user_ptr::try_bytes_mut(buf, len).is_err() {
        return negative(EFAULT);
    }
    let mut staging = Vec::new();
    if staging.try_reserve_exact(len).is_err() {
        return negative(EIO);
    }
    staging.resize(len, 0u8);
    if let Err(error) = raw_read(id, me, lba, &mut staging) {
        return errno(error);
    }
    match user_ptr::try_copy_to(buf, &staging) {
        Ok(()) => len as u64,
        Err(_) => negative(EFAULT),
    }
}

fn write(me: usize, id: usize, lba: u64, buf: u64, bytes: u64) -> u64 {
    let Some(len) = length(bytes) else {
        return negative(EINVAL);
    };
    let Ok(data) = user_ptr::try_read_vec(buf, len) else {
        return negative(EFAULT);
    };
    match raw_write(id, me, lba, &data) {
        Ok(()) => len as u64,
        Err(error) => errno(error),
    }
}
