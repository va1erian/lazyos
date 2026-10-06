//! The raw input event bus (`docs/input-plan.md`), wrapping syscall 25.
//!
//! Only a task holding `CAP_INPUT_RAW` (`inputd`) may read it; an input
//! driver holding `CAP_INPUT_SOURCE` may publish onto it as a registered
//! source (`docs/usb-hid-plan.md` U1); everyone else gets `-EPERM`.

use core::arch::asm;

/// `input_raw(op, a1, a2)`: the raw input event bus.
pub const SYS_INPUT_RAW: u64 = 25;

/// Bytes per raw event record.
pub const RAW_EVENT_BYTES: usize = 24;

/// Capability bit that authorises the raw bus (kernel `ipc::credentials`).
pub const CAP_INPUT_RAW: u32 = 1 << 9;

/// Capability bit that authorises publishing as a source.
pub const CAP_INPUT_SOURCE: u32 = 1 << 10;

/// Capability bit that authorises claiming the login console's keyboard
/// (issue #396): `init` stamps it onto `logind` alone.
pub const CAP_INPUT_CONSOLE: u32 = 1 << 13;

/// Source classes (`kernel/src/input/sources.rs`): what a source may publish.
pub mod source_class {
    /// `KEY` records.
    pub const KEYBOARD: u8 = 1;
    /// `REL_MOTION`, `BUTTON`, `SCROLL`.
    pub const POINTER: u8 = 2;
    /// `ABS_MOTION`, `BUTTON`, `SCROLL`.
    pub const TABLET: u8 = 3;
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
}

/// Raw event kinds (`kernel/src/input/bus.rs`).
pub mod raw_kind {
    pub const KEY: u8 = 1;
    pub const REL_MOTION: u8 = 2;
    pub const ABS_MOTION: u8 = 3;
    pub const BUTTON: u8 = 4;
    pub const SCROLL: u8 = 5;
    pub const DROPPED: u8 = 7;
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

fn input_syscall(op: u64, a1: u64, a2: u64) -> i64 {
    let code: u64;
    // SAFETY: `int 0x80` with syscall 25; the kernel validates the buffer and
    // capability, and clobbers only the registers declared here.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_INPUT_RAW,
            in("rdi") op,
            in("rsi") a1,
            in("rdx") a2,
            lateout("rax") code,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    code as i64
}

/// Claim this task's raw-bus consumer ring. `Err(-EPERM)` without `CAP_INPUT_RAW`.
pub fn input_raw_open() -> Result<(), i64> {
    match input_syscall(input_op::OPEN, 0, 0) {
        0 => Ok(()),
        code => Err(code),
    }
}

/// Drain queued raw events into `buf`, returning the number of whole records.
pub fn input_raw_poll(buf: &mut [u8]) -> Result<usize, i64> {
    let code = input_syscall(input_op::POLL, buf.as_mut_ptr() as u64, buf.len() as u64);
    if code >= 0 {
        Ok(code as usize)
    } else {
        Err(code)
    }
}

/// Release the consumer ring.
pub fn input_raw_close() -> Result<(), i64> {
    match input_syscall(input_op::CLOSE, 0, 0) {
        0 => Ok(()),
        code => Err(code),
    }
}

/// The task slot holding the display grant (the compositor), `Err(-ENOENT)`
/// when nothing is bound. Needs `CAP_INPUT_RAW`.
pub fn input_display_owner() -> Result<u64, i64> {
    let code = input_syscall(input_op::DISPLAY_OWNER, 0, 0);
    if code >= 0 {
        Ok(code as u64)
    } else {
        Err(code)
    }
}

/// Register a source of `class` ([`source_class`]); returns its id.
/// `Err(-EPERM)` without `CAP_INPUT_SOURCE`, `Err(-EBUSY)` when full.
pub fn input_source_register(class: u8) -> Result<u64, i64> {
    let code = input_syscall(input_op::REGISTER_SOURCE, u64::from(class), 0);
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
    let code = input_syscall(input_op::PUBLISH, bytes.as_ptr() as u64, packed);
    if code >= 0 {
        Ok(code as usize)
    } else {
        Err(code)
    }
}

/// Close source `id`; the kernel releases every key and button it held.
pub fn input_source_close(id: u64) -> Result<(), i64> {
    match input_syscall(input_op::CLOSE_SOURCE, id, 0) {
        0 => Ok(()),
        code => Err(code),
    }
}

/// Claim the login console's keyboard (issue #396): while the claim stands,
/// typed keys stay off the kernel terminal queue and `inputd` delivers them
/// to this task's sessionless input session. `Err(-EPERM)` without
/// `CAP_INPUT_CONSOLE`, `Err(-EBUSY)` while another task holds it.
pub fn input_console_claim() -> Result<(), i64> {
    match input_syscall(input_op::CONSOLE_CLAIM, 0, 0) {
        0 => Ok(()),
        code => Err(code),
    }
}

/// Give the console's keyboard back to the kernel terminal.
pub fn input_console_release() -> Result<(), i64> {
    match input_syscall(input_op::CONSOLE_RELEASE, 0, 0) {
        0 => Ok(()),
        code => Err(code),
    }
}

/// The task slot holding the console claim, `Err(-ENOENT)` when none does.
/// Needs `CAP_INPUT_RAW` (`inputd` authenticates its console client with it).
pub fn input_console_owner() -> Result<u64, i64> {
    let code = input_syscall(input_op::CONSOLE_OWNER, 0, 0);
    if code >= 0 {
        Ok(code as u64)
    } else {
        Err(code)
    }
}
