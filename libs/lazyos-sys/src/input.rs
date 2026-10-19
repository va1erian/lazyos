//! The raw input event bus (`docs/input-plan.md`), wrapping syscall 25.
//!
//! Only a task holding `CAP_INPUT_RAW` (`inputd`) may read it; an input
//! driver holding `CAP_INPUT_SOURCE` may publish onto it as a registered
//! source (`docs/usb-hid-plan.md` U1); everyone else gets `-EPERM`.

use crate::nr;

/// Bytes per raw event record.
pub const RAW_EVENT_BYTES: usize = 24;

pub use crate::cred::{CAP_INPUT_CONSOLE, CAP_INPUT_RAW, CAP_INPUT_SOURCE};

/// Source classes (`kernel/src/input/sources.rs`): what a source may publish.
pub mod source_class {
    /// `KEY` records.
    pub const KEYBOARD: u8 = 1;
    /// `REL_MOTION`, `BUTTON`, `SCROLL`.
    pub const POINTER: u8 = 2;
    /// `ABS_MOTION`, `BUTTON`, `SCROLL`.
    pub const TABLET: u8 = 3;
    /// `AXIS`, `BUTTON` (usages 1..=32).
    pub const GAMEPAD: u8 = 4;
}

/// Records one [`input_source_publish`] call may carry.
pub const SOURCE_MAX_BATCH: usize = 64;

/// One record a source publishes; the kernel adds sequence, time and device.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceRecord {
    pub kind: u8,
    pub code: u16,
    pub value: i32,
}

/// Raw-bus op codes, mirroring `kernel/src/input/rawsys.rs`.
pub mod input_op {
    pub const OPEN: u64 = 0;
    pub const POLL: u64 = 1;
    pub const CLOSE: u64 = 2;
    pub const DISPLAY_OWNER: u64 = 3;
    pub const REGISTER_SOURCE: u64 = 4;
    pub const PUBLISH: u64 = 5;
    pub const CLOSE_SOURCE: u64 = 6;
    pub const CONSOLE_CLAIM: u64 = 7;
    pub const CONSOLE_RELEASE: u64 = 8;
    pub const CONSOLE_OWNER: u64 = 9;
    /// The source table ([`input_raw_sources`]).
    pub const SOURCES: u64 = 10;
}

/// Raw event kinds (`kernel/src/input/bus.rs`).
pub mod raw_kind {
    pub const KEY: u8 = 1;
    pub const REL_MOTION: u8 = 2;
    pub const ABS_MOTION: u8 = 3;
    pub const BUTTON: u8 = 4;
    pub const SCROLL: u8 = 5;
    pub const DROPPED: u8 = 7;
    /// A gamepad axis (`kernel/src/input/bus_axis.rs`): `code` is its Generic
    /// Desktop usage, `value` the normalized value.
    pub const AXIS: u8 = 8;
    /// A device's identity: `code` = vendor id, `value` = product and version.
    pub const DEVICE: u8 = 9;
    /// Kernel-originated, never a driver's: source `device` registered
    /// (`value` 1) or closed (`value` 0, after its releases); `code` is its
    /// [`source_class`](super::source_class).
    pub const SOURCE: u8 = 10;
}

/// Source slots the kernel holds (`kernel/src/input/sources.rs`).
pub const MAX_SOURCES: usize = 16;
/// Bytes per entry [`input_raw_sources`] reads: device, class, generation
/// (little-endian `u16`).
pub const SOURCE_ENTRY_BYTES: usize = 4;
/// Bytes of the header before the entries: the bus's next sequence number
/// (little-endian `u64`).
pub const SOURCE_HEADER_BYTES: usize = 8;

/// What [`input_raw_sources`] found besides the entries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceTable {
    /// The bus's next sequence number when the table was read: every record
    /// with a smaller `seq` was published before it.
    pub next_seq: u64,
    /// Entries written to the caller's array.
    pub count: usize,
}

/// One source, as [`input_raw_sources`] reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceInfo {
    /// The device id its records carry.
    pub device: u8,
    /// Its [`source_class`].
    pub class: u8,
    /// Changes every time the slot is registered, so a reused device id is
    /// told apart from the source that held it before.
    pub generation: u16,
}

/// One decoded raw event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawEvent {
    pub seq: u64,
    pub ts_ns: u64,
    pub device: u8,
    pub kind: u8,
    /// HID usage for [`raw_kind::KEY`].
    pub code: u16,
    /// `1` press / `0` release; for [`raw_kind::DROPPED`] the number lost.
    pub value: i32,
}

impl RawEvent {
    /// Decode record `index` of `bytes`, `None` past the end.
    pub fn decode(bytes: &[u8], index: usize) -> Option<RawEvent> {
        let at = index.checked_mul(RAW_EVENT_BYTES)?;
        let record = bytes.get(at..at.checked_add(RAW_EVENT_BYTES)?)?;
        Some(RawEvent {
            seq: u64::from_le_bytes(record[0..8].try_into().ok()?),
            ts_ns: u64::from_le_bytes(record[8..16].try_into().ok()?),
            device: record[16],
            kind: record[17],
            code: u16::from_le_bytes(record[18..20].try_into().ok()?),
            value: i32::from_le_bytes(record[20..24].try_into().ok()?),
        })
    }
}

/// One raw-bus op. Ops that take a buffer pass it as `a1`.
///
/// # Safety
///
/// `a1`, when `op` reads or writes through it, must be valid for that access.
unsafe fn input_syscall(op: u64, a1: u64, a2: u64) -> i64 {
    // SAFETY: forwarded; the caller upholds the pointer contract.
    unsafe { crate::raw::syscall3(nr::INPUT_RAW, op, a1, a2) }
}

/// Claim this task's raw-bus consumer ring. `Err(-EPERM)` without `CAP_INPUT_RAW`.
pub fn input_raw_open() -> Result<(), i64> {
    // SAFETY: no pointer crosses the gate.
    match unsafe { input_syscall(input_op::OPEN, 0, 0) } {
        0 => Ok(()),
        code => Err(code),
    }
}

/// Drain queued raw events into `buf`, returning the number of whole records.
pub fn input_raw_poll(buf: &mut [u8]) -> Result<usize, i64> {
    // SAFETY: the kernel writes at most `buf.len()` bytes into `buf`.
    let code = unsafe { input_syscall(input_op::POLL, buf.as_mut_ptr() as u64, buf.len() as u64) };
    if code >= 0 {
        Ok(code as usize)
    } else {
        Err(code)
    }
}

/// Release the consumer ring.
pub fn input_raw_close() -> Result<(), i64> {
    // SAFETY: no pointer crosses the gate.
    match unsafe { input_syscall(input_op::CLOSE, 0, 0) } {
        0 => Ok(()),
        code => Err(code),
    }
}

/// The task slot holding the display grant (the compositor), `Err(-ENOENT)`
/// when nothing is bound. Needs `CAP_INPUT_RAW`.
pub fn input_display_owner() -> Result<u64, i64> {
    // SAFETY: no pointer crosses the gate.
    let code = unsafe { input_syscall(input_op::DISPLAY_OWNER, 0, 0) };
    if code >= 0 {
        Ok(code as u64)
    } else {
        Err(code)
    }
}

/// Register a source of `class` ([`source_class`]); returns its id.
/// `Err(-EPERM)` without `CAP_INPUT_SOURCE`, `Err(-EBUSY)` when full.
pub fn input_source_register(class: u8) -> Result<u64, i64> {
    // SAFETY: no pointer crosses the gate.
    let code = unsafe { input_syscall(input_op::REGISTER_SOURCE, u64::from(class), 0) };
    if code >= 0 {
        Ok(code as u64)
    } else {
        Err(code)
    }
}

/// Publish up to [`SOURCE_MAX_BATCH`] records from source `id`; returns how
/// many the kernel accepted (the rest were the wrong kind for the class, out
/// of range, or over the rate limit).
pub fn input_source_publish(id: u64, records: &[SourceRecord]) -> Result<usize, i64> {
    if records.is_empty() || records.len() > SOURCE_MAX_BATCH {
        return Err(-22);
    }
    let mut bytes = [0u8; SOURCE_MAX_BATCH * 8];
    for (record, out) in records.iter().zip(bytes.as_chunks_mut::<8>().0) {
        out[0] = record.kind;
        out[2..4].copy_from_slice(&record.code.to_le_bytes());
        out[4..8].copy_from_slice(&record.value.to_le_bytes());
    }
    let packed = id << 16 | records.len() as u64;
    // SAFETY: the kernel reads `records.len()` 8-byte records from `bytes`.
    let code = unsafe { input_syscall(input_op::PUBLISH, bytes.as_ptr() as u64, packed) };
    if code >= 0 {
        Ok(code as usize)
    } else {
        Err(code)
    }
}

/// Close source `id`; the kernel releases every key and button it held.
pub fn input_source_close(id: u64) -> Result<(), i64> {
    // SAFETY: no pointer crosses the gate.
    match unsafe { input_syscall(input_op::CLOSE_SOURCE, id, 0) } {
        0 => Ok(()),
        code => Err(code),
    }
}

/// Claim the login console's keyboard (issue #396): while the claim stands,
/// typed keys stay off the kernel terminal queue and `inputd` delivers them
/// to this task's sessionless input session. `Err(-EPERM)` without
/// `CAP_INPUT_CONSOLE`, `Err(-EBUSY)` while another task holds it.
pub fn input_console_claim() -> Result<(), i64> {
    // SAFETY: no pointer crosses the gate.
    match unsafe { input_syscall(input_op::CONSOLE_CLAIM, 0, 0) } {
        0 => Ok(()),
        code => Err(code),
    }
}

/// Give the console's keyboard back to the kernel terminal.
pub fn input_console_release() -> Result<(), i64> {
    // SAFETY: no pointer crosses the gate.
    match unsafe { input_syscall(input_op::CONSOLE_RELEASE, 0, 0) } {
        0 => Ok(()),
        code => Err(code),
    }
}

/// The task slot holding the console claim, `Err(-ENOENT)` when none does.
/// Needs `CAP_INPUT_RAW` (`inputd` authenticates its console client with it).
pub fn input_console_owner() -> Result<u64, i64> {
    // SAFETY: no pointer crosses the gate.
    let code = unsafe { input_syscall(input_op::CONSOLE_OWNER, 0, 0) };
    if code >= 0 {
        Ok(code as u64)
    } else {
        Err(code)
    }
}

/// The source table, in slot order, written into `out`: how many entries
/// there are, and the bus's next sequence number read together with them, so
/// **every record with `seq` below it was published before the table was
/// read, and every `SOURCE` record among them is reflected in it**. A
/// record-stream consumer applies the table when its drain reaches that
/// point (not when it reads it: records queued before the read are still in
/// its ring) and re-reads it when it opens the bus and after every `DROPPED`,
/// since a `SOURCE` record can be lost in an overflow. A source stays listed
/// until its `SOURCE` 0 record is published, even if its driver died. Needs
/// `CAP_INPUT_RAW`.
pub fn input_raw_sources(out: &mut [SourceInfo; MAX_SOURCES]) -> Result<SourceTable, i64> {
    let mut bytes = [0u8; SOURCE_HEADER_BYTES + MAX_SOURCES * SOURCE_ENTRY_BYTES];
    // SAFETY: the kernel writes at most `bytes.len()` bytes into `bytes`.
    let code = unsafe {
        input_syscall(
            input_op::SOURCES,
            bytes.as_mut_ptr() as u64,
            bytes.len() as u64,
        )
    };
    if code < 0 {
        return Err(code);
    }
    Ok(decode_source_table(&bytes, code as usize, out))
}

/// Decode what the `SOURCES` op wrote: `count` entries after the header.
/// A count beyond what the buffer or `out` holds is clamped.
pub fn decode_source_table(
    bytes: &[u8],
    count: usize,
    out: &mut [SourceInfo; MAX_SOURCES],
) -> SourceTable {
    let mut header = [0u8; SOURCE_HEADER_BYTES];
    if let Some(head) = bytes.get(..SOURCE_HEADER_BYTES) {
        header.copy_from_slice(head);
    }
    let body = bytes.get(SOURCE_HEADER_BYTES..).unwrap_or(&[]);
    let mut written = 0;
    for (entry, info) in body
        .as_chunks::<SOURCE_ENTRY_BYTES>()
        .0
        .iter()
        .take(count)
        .zip(out.iter_mut())
    {
        *info = SourceInfo {
            device: entry[0],
            class: entry[1],
            generation: u16::from_le_bytes([entry[2], entry[3]]),
        };
        written += 1;
    }
    SourceTable {
        next_seq: u64::from_le_bytes(header),
        count: written,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_header_and_entries() {
        let mut bytes = [0u8; SOURCE_HEADER_BYTES + 3 * SOURCE_ENTRY_BYTES];
        bytes[..8].copy_from_slice(&0x0102_0304_0506_0708u64.to_le_bytes());
        bytes[8..12].copy_from_slice(&[0x10, 4, 0x34, 0x12]);
        bytes[12..16].copy_from_slice(&[0x11, 2, 1, 0]);
        let mut out = [SourceInfo::default(); MAX_SOURCES];
        let table = decode_source_table(&bytes, 2, &mut out);
        assert_eq!(table.next_seq, 0x0102_0304_0506_0708);
        assert_eq!(table.count, 2);
        assert_eq!(
            out[0],
            SourceInfo {
                device: 0x10,
                class: 4,
                generation: 0x1234
            }
        );
        assert_eq!(out[1].device, 0x11);
        assert_eq!(out[2], SourceInfo::default());
    }

    #[test]
    fn clamps_a_count_beyond_the_buffer() {
        let bytes = [0u8; SOURCE_HEADER_BYTES + SOURCE_ENTRY_BYTES];
        let mut out = [SourceInfo::default(); MAX_SOURCES];
        assert_eq!(decode_source_table(&bytes, 99, &mut out).count, 1);
        assert_eq!(decode_source_table(&bytes[..4], 5, &mut out).count, 0);
        assert_eq!(decode_source_table(&[], 5, &mut out).next_seq, 0);
    }
}
