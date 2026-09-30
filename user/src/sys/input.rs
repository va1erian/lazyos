//! The raw input event bus (`docs/input-plan.md`), wrapping syscall 25.
//!
//! Only a task holding `CAP_INPUT_RAW` (`inputd`) may use it; everyone else
//! gets `-EPERM`.

use core::arch::asm;

/// `input_raw(op, a1, a2)`: the raw input event bus.
pub const SYS_INPUT_RAW: u64 = 25;

/// Bytes per raw event record.
pub const RAW_EVENT_BYTES: usize = 24;

/// Capability bit that authorises the raw bus (kernel `ipc::credentials`).
pub const CAP_INPUT_RAW: u32 = 1 << 9;

/// Raw-bus op codes, mirroring `kernel/src/input/rawsys.rs`.
pub mod input_op {
    pub const OPEN: u64 = 0;
    pub const POLL: u64 = 1;
    pub const CLOSE: u64 = 2;
}

/// Raw event kinds (`kernel/src/input/bus.rs`).
pub mod raw_kind {
    pub const KEY: u8 = 1;
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
