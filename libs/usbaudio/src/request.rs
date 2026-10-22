//! Payloads of the audio class requests `usbd` sends (UAC 1.0 5.2.3.2.3,
//! UAC 2.0 5.2.1, 5.2.3): the setup packets themselves are built by
//! `xhci::setup::request::audio`.

use crate::desc::{Range, RateSet, MAX_RANGES};
use crate::Error;

/// A UAC 1.0 Sampling Frequency control value: 3 bytes, little-endian Hz.
pub fn rate_uac1(rate: u32) -> [u8; 3] {
    let [a, b, c, _] = rate.to_le_bytes();
    [a, b, c]
}

/// A UAC 2.0 `CS_SAM_FREQ_CONTROL` CUR value: 4 bytes, little-endian Hz.
pub fn rate_uac2(rate: u32) -> [u8; 4] {
    rate.to_le_bytes()
}

/// A rate read back with GET_CUR (3 or 4 bytes).
pub fn parse_rate(bytes: &[u8]) -> Option<u32> {
    match bytes.len() {
        3 => Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0])),
        n if n >= 4 => Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
        _ => None,
    }
}

/// Bytes of a `GET_RANGE` reply with `count` subranges of 32-bit values.
pub fn range_reply_len(count: u16) -> usize {
    2 + usize::from(count) * 12
}

/// How many subranges a `GET_RANGE` reply holds (its first two bytes).
pub fn range_count(head: &[u8]) -> Result<u16, Error> {
    match head {
        [lo, hi, ..] => Ok(u16::from_le_bytes([*lo, *hi]).min(MAX_RANGES as u16)),
        _ => Err(Error::Short),
    }
}

/// A UAC 2.0 layout-3 `GET_RANGE` reply (`wNumSubRanges`, then MIN, MAX,
/// RES as 32-bit values) as a rate set; at most [`MAX_RANGES`] subranges.
pub fn parse_ranges(bytes: &[u8]) -> Result<RateSet, Error> {
    let count = range_count(bytes)?;
    let mut rates = RateSet::default();
    for index in 0..usize::from(count) {
        let at = 2 + index * 12;
        let field = |offset: usize| -> Result<u32, Error> {
            let b = bytes.get(at + offset..at + offset + 4).ok_or(Error::Short)?;
            Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        let (min, max, step) = (field(0)?, field(4)?, field(8)?);
        rates.push(Range {
            min,
            max,
            step: if min == max { 0 } else { step },
        });
    }
    Ok(rates)
}
