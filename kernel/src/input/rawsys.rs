//! Syscall 25: the raw input event bus (`docs/input-plan.md`, layer 1).
//!
//! ```text
//!   op 0 (open):  claim a consumer ring; -EPERM without CAP_INPUT_RAW
//!   op 1 (poll):  rsi -> record buffer, rdx = capacity in bytes -> count
//!   op 2 (close): release the ring
//!   op 3 (display_owner): the task slot holding the display grant, or -ENOENT
//!   op 4 (register_source): rsi = class -> source id; -EPERM without
//!        CAP_INPUT_SOURCE, -EINVAL bad class, -EBUSY table full
//!   op 5 (publish): rsi -> records, rdx = source id << 16 | count -> records
//!        accepted; -EBADF not the caller's source, -EINVAL count, -EFAULT
//!   op 6 (close_source): rsi = source id; releases what it held
//! ```
//!
//! Every return is a count/zero or `-errno`. Consumer records are
//! [`RAW_EVENT_BYTES`]-byte little-endian `RawEvent`s; published records are
//! [`sources::RECORD_BYTES`]-byte `(kind, 0, code, value)` and the kernel adds
//! the sequence number, time and device id. Ops 0-3 are gated on
//! `CAP_INPUT_RAW` so ambient authority no longer grants a keylogger: only the
//! task `init` stamps the bit onto (`inputd`) can read the stream. Ops 4-6 are
//! gated on `CAP_INPUT_SOURCE` (`docs/usb-hid-plan.md` U1), held by input
//! drivers; neither bit implies the other.

use alloc::vec::Vec;

use super::bus::{self, RAW_EVENT_BYTES};
use super::sources::{self, Record, MAX_BATCH, RECORD_BYTES};
use crate::ipc::credentials::{self, CAP_INPUT_RAW, CAP_INPUT_SOURCE};
use crate::{task, user_ptr};

pub mod op {
    pub const OPEN: u64 = 0;
    pub const POLL: u64 = 1;
    pub const CLOSE: u64 = 2;
    /// The compositor's task slot (the display grant holder).
    pub const DISPLAY_OWNER: u64 = 3;
    pub const REGISTER_SOURCE: u64 = 4;
    pub const PUBLISH: u64 = 5;
    pub const CLOSE_SOURCE: u64 = 6;
}

const EPERM: i64 = 1;
const ENOENT: i64 = 2;
const EFAULT: i64 = 14;
const EBUSY: i64 = 16;
const EINVAL: i64 = 22;
const EBADF: i64 = 9;

fn negative(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

/// The syscall entry point.
pub fn dispatch(operation: u64, buf: u64, capacity: u64) -> u64 {
    let me = task::current();
    let needed = match operation {
        op::REGISTER_SOURCE | op::PUBLISH | op::CLOSE_SOURCE => CAP_INPUT_SOURCE,
        _ => CAP_INPUT_RAW,
    };
    if me == task::KERNEL_TASK || !credentials::of(me).has_cap(needed) {
        return negative(EPERM);
    }
    match operation {
        op::OPEN => match bus::open(me) {
            Ok(_) => 0,
            Err(_) => negative(EBUSY),
        },
        op::POLL => poll(me, buf, capacity),
        op::CLOSE => {
            let closed = bus::consumer_of(me).map(|id| bus::close(id, me));
            match closed {
                Some(Ok(())) => 0,
                _ => negative(EBADF),
            }
        }
        // `inputd` accepts its shell interface only from the display grant's
        // holder; the kernel is the authority on who that is.
        op::DISPLAY_OWNER => match crate::display::owner() {
            Some(slot) => slot as u64,
            None => negative(ENOENT),
        },
        op::REGISTER_SOURCE => match u8::try_from(buf).map(|class| sources::register(me, class)) {
            Ok(Ok(id)) => id,
            Ok(Err(sources::Error::Full)) => negative(EBUSY),
            _ => negative(EINVAL),
        },
        op::PUBLISH => publish(me, buf, capacity),
        op::CLOSE_SOURCE => match sources::close(buf, me) {
            Ok(()) => 0,
            Err(_) => negative(EBADF),
        },
        _ => negative(EINVAL),
    }
}

/// Copy a batch of records in and publish them from the caller's source.
/// Nothing is published unless the whole buffer could be read.
fn publish(me: usize, ptr: u64, packed: u64) -> u64 {
    let (id, count) = (packed >> 16, (packed & 0xFFFF) as usize);
    if count == 0 || count > MAX_BATCH {
        return negative(EINVAL);
    }
    let len = count * RECORD_BYTES;
    let mut bytes = [0u8; MAX_BATCH * RECORD_BYTES];
    let bytes = &mut bytes[..len];
    if ptr == 0 {
        return negative(EFAULT);
    }
    // Copy out of user memory once and decode the copy.
    match user_ptr::try_bytes(ptr, len) {
        Ok(user) => bytes.copy_from_slice(user),
        Err(_) => return negative(EFAULT),
    }
    let mut records = [Record {
        kind: 0,
        code: 0,
        value: 0,
    }; MAX_BATCH];
    for (index, record) in records[..count].iter_mut().enumerate() {
        if let Some(decoded) = Record::decode(bytes, index) {
            *record = decoded;
        }
    }
    match sources::publish(id, me, &records[..count]) {
        Ok(outcome) => outcome.accepted as u64,
        Err(_) => negative(EBADF),
    }
}

/// Drain the caller's ring into its buffer.
fn poll(me: usize, ptr: u64, capacity: u64) -> u64 {
    let Some(id) = bus::consumer_of(me) else {
        return negative(EBADF);
    };
    if ptr == 0 {
        return negative(EFAULT);
    }
    // One drain yields at most the ring plus one `Dropped` marker; never size
    // kernel allocations from a caller-chosen capacity.
    let slots = (capacity / RAW_EVENT_BYTES as u64).min(bus::RING_CAP as u64 + 1) as usize;
    if slots == 0 {
        return 0;
    }
    // Prove the destination is writable *before* popping, so a bad buffer
    // cannot swallow input. Syscalls run to completion with interrupts off, so
    // nothing can unmap it between this probe and the copy.
    let probe = alloc::vec![0u8; slots * RAW_EVENT_BYTES];
    if user_ptr::try_copy_to(ptr, &probe).is_err() {
        return negative(EFAULT);
    }
    let mut events = Vec::with_capacity(slots);
    if bus::drain(id, me, slots, &mut events).is_err() {
        return negative(EBADF);
    }
    let mut encoded = Vec::with_capacity(events.len() * RAW_EVENT_BYTES);
    for event in &events {
        encoded.extend_from_slice(&event.to_bytes());
    }
    if user_ptr::try_copy_to(ptr, &encoded).is_err() {
        return negative(EFAULT);
    }
    events.len() as u64
}
