//! Syscall 35: the user-space filesystem surface. The contract (operations,
//! records, payloads) is `libs/fused` (`fused::wire`); this is its gate.
//!
//! Every operation needs `CAP_FS_PROVIDER`, and a provider only answers the
//! task that registered it. Nothing the daemon sends is trusted: names and
//! flags are checked, the request record must be writable before a request
//! is taken for it, a reply's data length is checked against the request's
//! room, and data moves only through the slot's bounce buffer ([`super`]).

use fused::wire::{
    sys_op, Reply, Request, FLAGS_ALL, FLAG_NOEXEC, FLAG_RO, MAX_MOUNT_NAME, REPLY_WORDS,
    REQUEST_WORDS,
};

use super::FuseError;
use crate::fs::vfs::MountFlags;
use crate::ipc::credentials::{self, CAP_FS_PROVIDER};
use crate::{task, user_ptr};

/// The longest a daemon may wait in one NEXT (1 s).
pub const MAX_WAIT_TICKS: u64 = 100;

const EPERM: i64 = 1;
const ENOENT: i64 = 2;
const ESRCH: i64 = 3;
const EFAULT: i64 = 14;
const EBUSY: i64 = 16;
const EINVAL: i64 = 22;
const ENOSPC: i64 = 28;
const ESTALE: i64 = 116;

fn negative(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

fn errno(error: FuseError) -> u64 {
    negative(match error {
        FuseError::Full => ENOSPC,
        FuseError::Busy => EBUSY,
        FuseError::NoMountRoot => ENOENT,
        FuseError::NotOwner => ESRCH,
        FuseError::Invalid => EINVAL,
        FuseError::Stale => ESTALE,
        FuseError::Fault => EFAULT,
    })
}

/// Whether task `me` may serve a filesystem.
pub fn may_provide(me: usize) -> bool {
    me != task::KERNEL_TASK && credentials::of(me).has_cap(CAP_FS_PROVIDER)
}

/// The syscall entry point.
pub fn dispatch(operation: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> u64 {
    let me = task::current();
    if !may_provide(me) {
        return negative(EPERM);
    }
    let id = || usize::try_from(a1).map_err(|_| negative(EINVAL));
    let result = match operation {
        sys_op::REGISTER => register(me, a1, a2, a3),
        sys_op::NEXT => id().map(|id| next(me, id, a2, a3, a4)),
        sys_op::REPLY => id().map(|id| reply(me, id, a2, a3)),
        sys_op::UNREGISTER => id().map(|id| match super::unregister(id, me) {
            Ok(()) => 0,
            Err(error) => errno(error),
        }),
        _ => Err(negative(EINVAL)),
    };
    result.unwrap_or_else(|code| code)
}

fn register(me: usize, name: u64, len: u64, flags: u64) -> Result<u64, u64> {
    let len = usize::try_from(len)
        .ok()
        .filter(|&len| (1..=MAX_MOUNT_NAME).contains(&len))
        .ok_or(negative(EINVAL))?;
    if flags & !FLAGS_ALL != 0 {
        return Err(negative(EINVAL));
    }
    let bytes = user_ptr::try_bytes(name, len).map_err(|_| negative(EFAULT))?;
    let name = core::str::from_utf8(bytes).map_err(|_| negative(EINVAL))?;
    let name = alloc::string::String::from(name);
    let flags = MountFlags {
        ro: flags & FLAG_RO != 0,
        noexec: flags & FLAG_NOEXEC != 0,
        nosuid: true,
    };
    match super::register(me, &name, flags) {
        Ok(id) => Ok(id as u64),
        Err(error) => Err(errno(error)),
    }
}

fn next(me: usize, id: usize, req: u64, data: u64, packed: u64) -> u64 {
    // The record must be writable before a request is taken for it.
    if user_ptr::try_copy_words(req, &[0; REQUEST_WORDS]).is_err() {
        return negative(EFAULT);
    }
    let cap = (packed & 0xFFFF_FFFF) as usize;
    let now = task::ticks();
    let deadline = (packed >> 32).clamp(now, now + MAX_WAIT_TICKS);
    let mut copy_out = |bytes: &[u8]| {
        if bytes.len() > cap {
            return Err(FuseError::Invalid);
        }
        user_ptr::try_copy_to(data, bytes).map_err(|_| FuseError::Fault)
    };
    match super::next(id, me, deadline, &mut copy_out) {
        Ok(Some(request)) => write_request(req, &request),
        Ok(None) => 0,
        Err(error) => errno(error),
    }
}

/// Hand the request record to the daemon (checked writable before the
/// request was taken; a daemon that unmaps it meanwhile never answers, and
/// the requester times out).
fn write_request(req: u64, request: &Request) -> u64 {
    match user_ptr::try_copy_words(req, &request.to_words()) {
        Ok(()) => 1,
        Err(_) => negative(EFAULT),
    }
}

fn reply(me: usize, id: usize, record: u64, data: u64) -> u64 {
    let mut words = [0u64; REPLY_WORDS];
    for (index, word) in words.iter_mut().enumerate() {
        match user_ptr::try_read_at::<u64>(record, index) {
            Ok(value) => *word = value,
            Err(_) => return negative(EFAULT),
        }
    }
    let answer = Reply::from_words(&words);
    let mut copy_in = |bounce: &mut [u8]| {
        let user = user_ptr::try_bytes(data, bounce.len()).map_err(|_| FuseError::Fault)?;
        bounce.copy_from_slice(user);
        Ok(())
    };
    match super::reply(id, me, &answer, &mut copy_in) {
        Ok(()) => 0,
        Err(error) => errno(error),
    }
}
