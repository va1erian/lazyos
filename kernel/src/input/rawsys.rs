//! Syscall 25: the raw input event bus (`docs/input-plan.md`, layer 1).
//!
//! ```text
//!   op 0 (open):  claim a consumer ring; -EPERM without CAP_INPUT_RAW
//!   op 1 (poll):  rsi -> record buffer, rdx = capacity in bytes -> count
//!   op 2 (close): release the ring
//!   op 3 (display_owner): the task slot holding the display grant, or -ENOENT
//!   op 4 (register_source): rsi = class -> source id; -EPERM without
//!        CAP_INPUT_SOURCE, -EINVAL bad class, -EBUSY table full; raises
//!        the caller to the Interactive scheduling class (never lowers it)
//!   op 5 (publish): rsi -> records, rdx = source id << 16 | count -> records
//!        accepted; -EBADF not the caller's source, -EINVAL count, -EFAULT
//!   op 6 (close_source): rsi = source id; releases what it held
//!   op 7 (console_claim): claim the login console's keyboard (issue #396,
//!        [`super::console`]); -EBUSY while another live task holds it
//!   op 8 (console_release): give it back; -EPERM when not the holder
//!   op 9 (console_owner): the claiming task's slot, or -ENOENT
//!   op 10 (sources): rsi -> buffer, rdx = capacity in bytes -> entries
//!        written, one [`sources::ENTRY_BYTES`] entry per live source in slot
//!        order (device, class, generation u16); -EFAULT
//! ```
//!
//! Every return is a count/zero or `-errno`. Consumer records are
//! [`RAW_EVENT_BYTES`]-byte little-endian `RawEvent`s; published records are
//! [`sources::RECORD_BYTES`]-byte `(kind, 0, code, value)` and the kernel adds
//! the sequence number, time and device id. Ops 0-3 are gated on
//! `CAP_INPUT_RAW` so ambient authority no longer grants a keylogger: only the
//! task `init` stamps the bit onto (`inputd`) can read the stream. Ops 4-6 are
//! gated on `CAP_INPUT_SOURCE` (`docs/usb-hid-plan.md` U1), held by input
//! drivers; neither bit implies the other. Ops 7-8 are gated on
//! `CAP_INPUT_CONSOLE` (`logind`); op 9, like op 3, on `CAP_INPUT_RAW`
//! (`inputd` authenticates its sessionless client with it); op 10 (the source
//! table, the truth behind the kernel's `SOURCE` records) is `CAP_INPUT_RAW`
//! too and needs no consumer ring.

use alloc::vec::Vec;

use super::bus::{self, RAW_EVENT_BYTES};
use super::console;
use super::sources::{self, Record, MAX_BATCH, RECORD_BYTES};
use crate::ipc::credentials::{self, CAP_INPUT_CONSOLE, CAP_INPUT_RAW, CAP_INPUT_SOURCE};
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
    /// Claim the login console's keyboard (`CAP_INPUT_CONSOLE`).
    pub const CONSOLE_CLAIM: u64 = 7;
    /// Release the claim.
    pub const CONSOLE_RELEASE: u64 = 8;
    /// The claiming task's slot (`CAP_INPUT_RAW`).
    pub const CONSOLE_OWNER: u64 = 9;
    /// The live source table (`CAP_INPUT_RAW`).
    pub const SOURCES: u64 = 10;
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
        op::CONSOLE_CLAIM | op::CONSOLE_RELEASE => CAP_INPUT_CONSOLE,
        _ => CAP_INPUT_RAW,
    };
    if me == task::KERNEL_TASK || !credentials::of(me).has_cap(needed) {
        return negative(EPERM);
    }
    match operation {
        op::OPEN => match bus::open(me) {
            Ok(_) => {
                // The consumer is on every keystroke's and pointer move's
                // path: its doorbell wake (P1.3) must preempt ordinary work,
                // as the sources' and the compositor's do.
                task::raise_priority(me, task::PriorityClass::Interactive);
                task::guard::mark_input(me);
                0
            }
            Err(_) => negative(EBUSY),
        },
        op::POLL => poll(me, buf, capacity),
        op::CLOSE => {
            let closed = bus::consumer_of(me).map(|id| bus::close(id, me));
            match closed {
                Some(Ok(())) => {
                    // Still the input path while it owns a source.
                    if !sources::owns_any(me) {
                        task::guard::unmark_input(me);
                    }
                    0
                }
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
            Ok(Ok(id)) => {
                // An input driver is latency-critical: a polled device (USB
                // HID) holds only a few events, and a driver left behind a
                // console repaint loses keys. Raise it to the class of the
                // multiplexer it would otherwise wait on, never lowering it.
                task::raise_priority(me, task::PriorityClass::Interactive);
                task::guard::mark_input(me);
                id
            }
            Ok(Err(sources::Error::Full)) => negative(EBUSY),
            _ => negative(EINVAL),
        },
        op::PUBLISH => publish(me, buf, capacity),
        op::CONSOLE_CLAIM => match console::claim(me) {
            Ok(()) => 0,
            Err(_) => negative(EBUSY),
        },
        op::CONSOLE_RELEASE => match console::release(me) {
            Ok(()) => 0,
            Err(_) => negative(EPERM),
        },
        op::CONSOLE_OWNER => match console::holder() {
            Some(slot) => slot as u64,
            None => negative(ENOENT),
        },
        op::SOURCES => source_table(buf, capacity),
        op::CLOSE_SOURCE => match sources::close(buf, me) {
            Ok(()) => {
                // One source closed (a mouse unplugged): a driver that still
                // owns another, or is the consumer, keeps its share.
                if !sources::owns_any(me) && bus::consumer_of(me).is_none() {
                    task::guard::unmark_input(me);
                }
                0
            }
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

/// Write the live source table into the caller's buffer, as many whole
/// entries as `capacity` holds; returns how many were written.
fn source_table(ptr: u64, capacity: u64) -> u64 {
    if ptr == 0 {
        return negative(EFAULT);
    }
    let (table, live) = sources::snapshot();
    let fit = (capacity / sources::ENTRY_BYTES as u64).min(live as u64) as usize;
    let mut bytes = [0u8; sources::MAX_SOURCES * sources::ENTRY_BYTES];
    for (info, out) in table[..fit]
        .iter()
        .zip(bytes.as_chunks_mut::<{ sources::ENTRY_BYTES }>().0)
    {
        *out = info.to_bytes();
    }
    if user_ptr::try_copy_to(ptr, &bytes[..fit * sources::ENTRY_BYTES]).is_err() {
        return negative(EFAULT);
    }
    fit as u64
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
    crate::perf::input_read(events.len());
    let mut encoded = Vec::with_capacity(events.len() * RAW_EVENT_BYTES);
    for event in &events {
        encoded.extend_from_slice(&event.to_bytes());
    }
    if user_ptr::try_copy_to(ptr, &encoded).is_err() {
        return negative(EFAULT);
    }
    events.len() as u64
}
