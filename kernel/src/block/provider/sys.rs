//! Syscall 32: the storage surface (docs/architecture/usb-storage.md).
//!
//! ```text
//!   rdi = op, rsi/rdx/r10/r8 = a1..a4; every return is a value or -errno
//!
//!   op 0 REGISTER(info)           info -> [u64; 4] {sectors, sector_size,
//!                                 flags (bit 0 writable), 0} -> disk id
//!   op 1 NEXT(id, req, data, cap|deadline << 32)
//!                                 wait until `deadline` (absolute ticks,
//!                                 capped at now + 1 s) for a request;
//!                                 writes req -> [u64; 4] {tag, op, lba,
//!                                 bytes}, a write's data to `data` (cap >=
//!                                 bytes) -> 1, or 0 when none came
//!   op 2 COMPLETE(id, tag, status, data)
//!                                 finish request `tag`; a successful read
//!                                 passes exactly `bytes` bytes at `data`
//!   op 3 REMOVE(id)               the device is gone: the disk dies
//!   op 4 SETTLE()                 scan new disks for partitions and mount
//!                                 the configured home volume if it is
//!                                 there (`fs::late`) -> its state
//!   op 5 SCANNED()                the provider registered every device
//!                                 present when it started -> 0
//! ```
//!
//! Ops 0-3 and 5 need `CAP_BLOCK_PROVIDER` and a block-provider uid
//! (`usbpolicy::BLOCK_PROVIDER_UIDS`: only `_usb`); a disk only answers the
//! task that registered it. Op 4 needs `CAP_SYS_ADMIN` (`init` calls it).
//! Nothing the provider sends is trusted: lengths are checked against the
//! request, tags against the request in flight, and data is copied through
//! the kernel's bounce buffer ([`super`]).

use super::{ProviderError, Request, MAX_REQUEST_BYTES};
use crate::block::SECTOR_SIZE;
use crate::ipc::credentials::{self, CAP_BLOCK_PROVIDER, CAP_SYS_ADMIN};
use crate::{task, user_ptr};

pub mod op {
    pub const REGISTER: u64 = 0;
    pub const NEXT: u64 = 1;
    pub const COMPLETE: u64 = 2;
    pub const REMOVE: u64 = 3;
    pub const SETTLE: u64 = 4;
    pub const SCANNED: u64 = 5;
}

/// Bit 0 of the REGISTER flags: the medium can be written.
pub const FLAG_WRITABLE: u64 = 1;
/// The longest a provider may wait in one NEXT (1 s).
pub const MAX_WAIT_TICKS: u64 = 100;

const EPERM: i64 = 1;
const ESRCH: i64 = 3;
const EFAULT: i64 = 14;
const EBUSY: i64 = 16;
const EINVAL: i64 = 22;
const ESTALE: i64 = 116;

fn negative(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

fn errno(error: ProviderError) -> u64 {
    negative(match error {
        ProviderError::Full => EBUSY,
        ProviderError::NotOwner => ESRCH,
        ProviderError::Invalid => EINVAL,
        ProviderError::Stale => ESTALE,
        ProviderError::Fault => EFAULT,
    })
}

/// Whether task `me` may serve block devices.
pub fn may_provide(me: usize) -> bool {
    let cred = credentials::of(me);
    me != task::KERNEL_TASK
        && cred.has_cap(CAP_BLOCK_PROVIDER)
        && usbpolicy::BLOCK_PROVIDER_UIDS.contains(&cred.uid)
}

/// The syscall entry point.
pub fn dispatch(operation: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> u64 {
    let me = task::current();
    if operation == op::SETTLE {
        if me == task::KERNEL_TASK || !credentials::of(me).has_cap(CAP_SYS_ADMIN) {
            return negative(EPERM);
        }
        return crate::fs::late::settle() as u64;
    }
    if !may_provide(me) {
        return negative(EPERM);
    }
    match operation {
        op::REGISTER => register(me, a1),
        op::NEXT => next(me, a1, a2, a3, a4),
        op::COMPLETE => complete(me, a1, a2, a3, a4),
        op::SCANNED => {
            crate::fs::late::provider_scanned();
            0
        }
        op::REMOVE => match usize::try_from(a1).map(|id| super::remove(id, me)) {
            Ok(Ok(())) => 0,
            Ok(Err(error)) => errno(error),
            Err(_) => negative(EINVAL),
        },
        _ => negative(EINVAL),
    }
}

fn register(me: usize, info: u64) -> u64 {
    let mut words = [0u64; 4];
    for (index, word) in words.iter_mut().enumerate() {
        match user_ptr::try_read_at::<u64>(info, index) {
            Ok(value) => *word = value,
            Err(_) => return negative(EFAULT),
        }
    }
    let [sectors, sector_size, flags, _] = words;
    if sector_size != SECTOR_SIZE as u64 || flags & !FLAG_WRITABLE != 0 {
        return negative(EINVAL);
    }
    match super::register(me, sectors, flags & FLAG_WRITABLE != 0) {
        Ok(id) => id as u64,
        Err(error) => errno(error),
    }
}

fn next(me: usize, id: u64, req: u64, data: u64, packed: u64) -> u64 {
    let Ok(id) = usize::try_from(id) else {
        return negative(EINVAL);
    };
    // The record must be writable before a request is taken for it.
    if user_ptr::try_copy_words(req, &[0; 4]).is_err() {
        return negative(EFAULT);
    }
    let cap = (packed & 0xFFFF_FFFF) as usize;
    let now = task::ticks();
    let deadline = (packed >> 32).clamp(now, now + MAX_WAIT_TICKS);
    let mut copy_out = |bytes: &[u8]| {
        if bytes.len() > cap {
            return Err(ProviderError::Invalid);
        }
        user_ptr::try_copy_to(data, bytes).map_err(|_| ProviderError::Fault)
    };
    match super::next(id, me, deadline, &mut copy_out) {
        Ok(Some(request)) => write_request(req, &request),
        Ok(None) => 0,
        Err(error) => errno(error),
    }
}

/// Hand the request record to the provider (checked writable before the
/// request was taken; a provider that unmaps it meanwhile never completes
/// the request, and the requester times out).
fn write_request(req: u64, request: &Request) -> u64 {
    let words = [
        request.tag,
        request.op as u64,
        request.lba,
        request.bytes as u64,
    ];
    match user_ptr::try_copy_words(req, &words) {
        Ok(()) => 1,
        Err(_) => negative(EFAULT),
    }
}

fn complete(me: usize, id: u64, tag: u64, code: u64, data: u64) -> u64 {
    let Ok(id) = usize::try_from(id) else {
        return negative(EINVAL);
    };
    let mut copy_in = |bounce: &mut [u8]| {
        if bounce.len() > MAX_REQUEST_BYTES {
            return Err(ProviderError::Invalid);
        }
        let user = user_ptr::try_bytes(data, bounce.len()).map_err(|_| ProviderError::Fault)?;
        bounce.copy_from_slice(user);
        Ok(())
    };
    match super::complete(id, me, tag, code, &mut copy_in) {
        Ok(()) => 0,
        Err(error) => errno(error),
    }
}
