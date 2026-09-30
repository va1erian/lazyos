//! Syscall 25: the raw input event bus (`docs/input-plan.md`, layer 1).
//!
//! ```text
//!   op 0 (open):  claim a consumer ring; -EPERM without CAP_INPUT_RAW
//!   op 1 (poll):  rsi -> record buffer, rdx = capacity in bytes -> count
//!   op 2 (close): release the ring
//!   op 3 (display_owner): the task slot holding the display grant, or -ENOENT
//! ```
//!
//! Every return is a count/zero or `-errno`. Records are
//! [`RAW_EVENT_BYTES`]-byte little-endian [`RawEvent`]s. Opening is gated on
//! `CAP_INPUT_RAW` so ambient authority no longer grants a keylogger: only the
//! task `init` stamps the bit onto (`inputd`) can read the stream.

use alloc::vec::Vec;

use super::bus::{self, RAW_EVENT_BYTES};
use crate::ipc::credentials::{self, CAP_INPUT_RAW};
use crate::{task, user_ptr};

pub mod op {
    pub const OPEN: u64 = 0;
    pub const POLL: u64 = 1;
    pub const CLOSE: u64 = 2;
    /// The compositor's task slot (the display grant holder).
    pub const DISPLAY_OWNER: u64 = 3;
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
    if me == task::KERNEL_TASK || !credentials::of(me).has_cap(CAP_INPUT_RAW) {
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
        _ => negative(EINVAL),
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
    let slots = (capacity / RAW_EVENT_BYTES as u64) as usize;
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
