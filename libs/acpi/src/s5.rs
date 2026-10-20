//! ACPI soft-off (S5): the `\_S5` sleep type and the register values that
//! enter it (ACPI 6.5, 4.8.3.2 and 7.3.4).
//!
//! The kernel carries no AML interpreter, so `\_S5` is found the way every
//! small OS does it: a bounded byte scan of the DSDT for `Name (\_S5, Package
//! () { SLP_TYPa, SLP_TYPb, ... })`, whose encoding is fixed
//! (`NameOp "_S5_" PackageOp PkgLength NumElements elements...`, each element
//! a `ZeroOp`, `OneOp` or `BytePrefix` constant). The DSDT is hostile input
//! like every other table: every read is bounded, a candidate that does not
//! decode is skipped, and a DSDT without a usable `\_S5` is an error, never a
//! guess (the caller then has the QEMU ports and a halt left).

use crate::sdt::{Sdt, HEADER_LEN};
use crate::{Error, PhysMem};

/// `SLP_TYP` is a 3-bit field in PM1 control and the sleep control register.
const SLP_TYP_MAX: u8 = 7;

/// PM1 control: `SLP_TYP` is bits 10..=12, `SLP_EN` bit 13.
pub const PM1_SLP_TYP_SHIFT: u32 = 10;
pub const PM1_SLP_TYP_MASK: u16 = 0b111 << PM1_SLP_TYP_SHIFT;
pub const PM1_SLP_EN: u16 = 1 << 13;
/// PM1 control `GBL_RLS` (bit 2) is write-only: a read may return garbage
/// there, and writing it back raises an SMI to the firmware (ACPI 4.8.3.2.1;
/// ACPICA masks `SLP_EN | GBL_RLS` after every read).
pub const PM1_GBL_RLS: u16 = 1 << 2;

/// Every fixed-hardware PM1 status bit that is write-one-to-clear: `TMR_STS`,
/// `BM_STS`, `GBL_STS`, `PWRBTN_STS`, `SLPBTN_STS`, `RTC_STS`,
/// `PCIEXP_WAKE_STS` and `WAK_STS`. Cleared before sleeping so a stale event
/// (the power button press that preceded a shutdown, a wake flag) cannot
/// wake the machine at once (ACPICA's `acpi_hw_clear_acpi_status`).
pub const PM1_STATUS_CLEAR: u16 = 0xC731;

/// PM1 blocks (a, b) a power-off writes at most.
pub const MAX_PM1_BLOCKS: usize = 2;

/// Sleep control register (hardware-reduced ACPI): `SLP_TYP` is bits 2..=4,
/// `SLP_EN` bit 5.
pub const SLEEP_CONTROL_SLP_TYP_SHIFT: u32 = 2;
pub const SLEEP_CONTROL_SLP_EN: u8 = 1 << 5;

/// How many bytes are examined per physical read while scanning.
const STRIDE: u32 = 512;
/// Longest encoding that follows the name: `PackageOp`, a 4-byte `PkgLength`,
/// `NumElements` and two 3-byte (WordPrefix) elements.
const AFTER_NAME: usize = 1 + 4 + 1 + 6;
/// Bytes read before a stride so the context in front of a name is visible.
const BEFORE: u32 = 2;
/// Read buffer: the context, a stride and the encoding after its last name.
const WINDOW: usize = BEFORE as usize + STRIDE as usize + 4 + AFTER_NAME;

/// The `SLP_TYP` values of the S5 state: `a` goes to PM1a control, `b` to
/// PM1b control (or the sleep control register uses `a`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SleepType {
    pub a: u8,
    pub b: u8,
}

/// Find `\_S5` in `dsdt`. `NotFound` when no candidate exists, `Malformed`
/// when candidates exist but none decodes, `Unreadable` when the table cannot
/// be read.
pub fn find_s5(mem: &dyn PhysMem, dsdt: &Sdt) -> Result<SleepType, Error> {
    let mut failure = Error::NotFound;
    let mut base = HEADER_LEN;
    while base < dsdt.length {
        let from = base - BEFORE.min(base);
        let want = (dsdt.length - from).min(WINDOW as u32) as usize;
        let mut buf = [0u8; WINDOW];
        if !mem.read(dsdt.phys + u64::from(from), &mut buf[..want]) {
            return Err(Error::Unreadable);
        }
        let buf = &buf[..want];
        let stride_end = (base + STRIDE).min(dsdt.length);
        for index in 0..buf.len().saturating_sub(3) {
            let offset = from + index as u32;
            if offset < base || offset >= stride_end || &buf[index..index + 4] != b"_S5_" {
                continue;
            }
            match decode(buf, index) {
                Some(sleep) => return Ok(sleep),
                None => failure = Error::Malformed,
            }
        }
        base = stride_end;
    }
    Err(failure)
}

/// Decode the package after the `_S5_` that starts at `name` in `buf`; `None`
/// when what surrounds it is not `Name (\_S5, Package () {a, b, ...})`.
fn decode(buf: &[u8], name: usize) -> Option<SleepType> {
    // NameOp, optionally with the root prefix `\` between it and the name.
    let named = match name.checked_sub(1).map(|i| buf[i]) {
        Some(0x08) => true,
        Some(0x5C) => name >= 2 && buf[name - 2] == 0x08,
        _ => false,
    };
    if !named {
        return None;
    }
    let mut at = name + 4;
    if *buf.get(at)? != 0x12 {
        return None;
    }
    // PkgLength: the top two bits of the lead byte count the bytes after it.
    at += 1;
    at += 1 + usize::from(*buf.get(at)? >> 6);
    let elements = *buf.get(at)?;
    at += 1;
    if elements < 2 {
        return None;
    }
    let (a, at) = constant(buf, at)?;
    let (b, _) = constant(buf, at)?;
    Some(SleepType { a, b })
}

/// One package element that is a small integer constant: `(value, next)`.
fn constant(buf: &[u8], at: usize) -> Option<(u8, usize)> {
    let (value, next) = match *buf.get(at)? {
        0x00 => (0, at + 1),
        0x01 => (1, at + 1),
        0x0A => (*buf.get(at + 1)?, at + 2),
        // WordPrefix: some compilers widen the constant; the high byte must
        // be zero for the value to be a legal SLP_TYP.
        0x0B if *buf.get(at + 2)? == 0 => (*buf.get(at + 1)?, at + 3),
        _ => return None,
    };
    (value <= SLP_TYP_MAX).then_some((value, next))
}

/// The two writes that enter S5 through a PM1 control register, given the
/// register's current value: first with `SLP_TYP` set, then with `SLP_EN` too
/// (the order Linux uses; other bits, notably `SCI_EN`, are kept; the
/// write-only `GBL_RLS` is not).
pub fn pm1_control_writes(current: u16, slp_typ: u8) -> (u16, u16) {
    let typ = u16::from(slp_typ & SLP_TYP_MAX) << PM1_SLP_TYP_SHIFT;
    let kept = current & !(PM1_SLP_TYP_MASK | PM1_SLP_EN | PM1_GBL_RLS);
    let prepared = kept | typ;
    (prepared, prepared | PM1_SLP_EN)
}

/// The ordered writes that enter S5 through the PM1 control blocks: each
/// block is `(current value, SLP_TYP)`. `SLP_TYP` goes to every block first,
/// then `SLP_EN` to every block; an entry is `(block index, value)` and the
/// count says how many are used (at most `2 * MAX_PM1_BLOCKS`).
pub fn pm1_enter_sequence(blocks: &[(u16, u8)]) -> ([(usize, u16); 2 * MAX_PM1_BLOCKS], usize) {
    let blocks = &blocks[..blocks.len().min(MAX_PM1_BLOCKS)];
    let mut out = [(0usize, 0u16); 2 * MAX_PM1_BLOCKS];
    for (index, &(current, typ)) in blocks.iter().enumerate() {
        let (prepared, enter) = pm1_control_writes(current, typ);
        out[index] = (index, prepared);
        out[blocks.len() + index] = (index, enter);
    }
    (out, blocks.len() * 2)
}

/// The value written to the sleep control register of a hardware-reduced
/// platform to enter S5.
pub fn sleep_control_value(slp_typ: u8) -> u8 {
    ((slp_typ & SLP_TYP_MAX) << SLEEP_CONTROL_SLP_TYP_SHIFT) | SLEEP_CONTROL_SLP_EN
}
