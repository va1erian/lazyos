//! The CMOS real-time clock (MC146818 and compatibles) on ports 0x70/0x71.
//!
//! Firmware and hypervisors keep it in UTC by convention (QEMU's default is
//! `-rtc base=utc`). It is read once at boot ([`crate::wallclock`]) and written
//! back on `clock_settime`; the decode/encode step is pure so the calendar
//! rules can be tested without the device.

use spin::Mutex;

use super::io::{inb, outb};
use crate::wallclock::{civil_from_days, days_from_civil, days_in_month};

const INDEX: u16 = 0x70;
const DATA: u16 = 0x71;

const REG_SECONDS: u8 = 0x00;
const REG_MINUTES: u8 = 0x02;
const REG_HOURS: u8 = 0x04;
const REG_DAY: u8 = 0x07;
const REG_MONTH: u8 = 0x08;
const REG_YEAR: u8 = 0x09;
const REG_STATUS_A: u8 = 0x0A;
const REG_STATUS_B: u8 = 0x0B;
/// The century register the ACPI FADT usually names (QEMU and most PCs).
const REG_CENTURY: u8 = 0x32;

/// Status A: an update is in progress and the time registers are unstable.
const A_UPDATE_IN_PROGRESS: u8 = 1 << 7;
/// Status B: the chip is halted for a write (`SET`).
const B_SET: u8 = 1 << 7;
/// Status B: 24-hour mode.
const B_24H: u8 = 1 << 1;
/// Status B: registers are binary rather than BCD.
const B_BINARY: u8 = 1 << 2;

/// Guard against a dead or absent chip that reports "updating" forever.
const UIP_SPINS: u32 = 2_000_000;

/// CMOS index/data is a two-step protocol; serialize it against itself.
static CMOS: Mutex<()> = Mutex::new(());

/// The raw time registers plus the status-B mode bits, exactly as read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Raw {
    pub second: u8,
    pub minute: u8,
    pub hour: u8,
    pub day: u8,
    pub month: u8,
    pub year: u8,
    pub century: u8,
    pub status_b: u8,
}

fn from_bcd(value: u8) -> Option<u8> {
    let (high, low) = (value >> 4, value & 0x0F);
    (high <= 9 && low <= 9).then_some(high * 10 + low)
}

fn to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

/// Decode `raw` to Unix seconds, or `None` if any field is out of range
/// (dead battery, uninitialised CMOS) so the caller can fall back.
pub fn decode(raw: Raw) -> Option<i64> {
    let bcd = raw.status_b & B_BINARY == 0;
    let field = |v: u8| if bcd { from_bcd(v) } else { Some(v) };
    let pm = raw.hour & 0x80 != 0;
    let mut hour = field(raw.hour & 0x7F)?;
    if raw.status_b & B_24H == 0 {
        // 12-hour clock: 1..=12 with the PM flag in bit 7.
        if !(1..=12).contains(&hour) {
            return None;
        }
        hour = hour % 12 + if pm { 12 } else { 0 };
    }
    let (second, minute) = (field(raw.second)?, field(raw.minute)?);
    let (day, month, year) = (field(raw.day)?, field(raw.month)?, field(raw.year)?);
    // A missing/garbage century register means "20xx", the only plausible one.
    let century = field(raw.century)
        .filter(|c| (19..=21).contains(c))
        .unwrap_or(20);
    let full_year = i64::from(century) * 100 + i64::from(year);
    if second > 59 || minute > 59 || hour > 23 || year > 99 {
        return None;
    }
    let (month, day) = (u32::from(month), u32::from(day));
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(full_year, month) {
        return None;
    }
    let days = days_from_civil(full_year, month, day);
    Some(days * 86_400 + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second))
}

/// Encode Unix seconds in the register format `status_b` describes (BCD or
/// binary, 12 or 24 hour), as a chip in that mode expects to be written.
pub fn encode(unix: i64, status_b: u8) -> Raw {
    let (year, month, day) = civil_from_days(unix.div_euclid(86_400));
    let secs = unix.rem_euclid(86_400) as u32;
    let (mut hour, minute, second) = (secs / 3600, (secs / 60) % 60, secs % 60);
    let mut pm = 0u8;
    if status_b & B_24H == 0 {
        pm = if hour >= 12 { 0x80 } else { 0 };
        hour = if hour % 12 == 0 { 12 } else { hour % 12 };
    }
    let out = |v: u32| {
        if status_b & B_BINARY == 0 {
            to_bcd(v as u8)
        } else {
            v as u8
        }
    };
    Raw {
        second: out(second),
        minute: out(minute),
        hour: out(hour) | pm,
        day: out(day),
        month: out(month),
        year: out((year % 100) as u32),
        century: out((year / 100) as u32),
        status_b,
    }
}

fn read_reg(reg: u8) -> u8 {
    // SAFETY: 0x70/0x71 is the CMOS index/data pair; selecting a register and
    // reading it has no side effect beyond the (idempotent) index latch, and
    // the caller holds `CMOS` so no other user interleaves the two steps.
    unsafe {
        outb(INDEX, reg);
        inb(DATA)
    }
}

fn write_reg(reg: u8, value: u8) {
    // SAFETY: as `read_reg`; the caller holds `CMOS` and has set `B_SET`, so
    // the chip is not mid-update while the register changes.
    unsafe {
        outb(INDEX, reg);
        outb(DATA, value);
    }
}

fn wait_stable() -> bool {
    (0..UIP_SPINS).any(|_| read_reg(REG_STATUS_A) & A_UPDATE_IN_PROGRESS == 0)
}

fn sample() -> Raw {
    Raw {
        second: read_reg(REG_SECONDS),
        minute: read_reg(REG_MINUTES),
        hour: read_reg(REG_HOURS),
        day: read_reg(REG_DAY),
        month: read_reg(REG_MONTH),
        year: read_reg(REG_YEAR),
        century: read_reg(REG_CENTURY),
        status_b: read_reg(REG_STATUS_B),
    }
}

/// Read the chip's current UTC time, or `None` if it never settles or holds
/// an impossible date. Reads twice until two samples agree, because a read
/// straddling the once-a-second update can tear.
pub fn read_unix() -> Option<i64> {
    let _guard = CMOS.lock();
    for _ in 0..8 {
        if !wait_stable() {
            return None;
        }
        let first = sample();
        if !wait_stable() {
            return None;
        }
        if first == sample() {
            return decode(first);
        }
    }
    None
}

/// Store `unix` in the chip. Best effort: an unsettable chip only means the
/// next boot starts from its old time.
pub fn write_unix(unix: i64) {
    let _guard = CMOS.lock();
    let status_b = read_reg(REG_STATUS_B);
    let raw = encode(unix, status_b);
    write_reg(REG_STATUS_B, status_b | B_SET);
    write_reg(REG_SECONDS, raw.second);
    write_reg(REG_MINUTES, raw.minute);
    write_reg(REG_HOURS, raw.hour);
    write_reg(REG_DAY, raw.day);
    write_reg(REG_MONTH, raw.month);
    write_reg(REG_YEAR, raw.year);
    write_reg(REG_CENTURY, raw.century);
    write_reg(REG_STATUS_B, status_b & !B_SET);
}
