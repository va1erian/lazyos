//! Frames per isochronous service interval (USB 2.0 5.12.4, UAC 1.0
//! 3.7.2.2, UAC 2.0 2.3.1.6): what each TD of a playback stream carries.
//!
//! A synchronous or adaptive endpoint takes the nominal rate: at 44.1 kHz
//! and one packet per millisecond that is 44 frames nine times and 45 the
//! tenth, which an exact rational accumulator gets right forever. An
//! asynchronous endpoint runs from its own clock and says, on its feedback
//! endpoint, how many frames per (micro)frame it wants; from the first
//! accepted report on, the pace follows it in 16.16 fixed point.
//!
//! Feedback formats are a known mess: full-speed devices should send 10.14
//! frames per frame in 3 bytes and high-speed ones 16.16 frames per
//! microframe in 4, and real devices get both wrong. A report is read in
//! the format of its speed, and if that is not within an eighth of the
//! nominal rate, shifted by up to four bits each way until it is (what Linux
//! does); a report no shift explains is ignored.

/// Microframes (125 us) in one second.
const MICROFRAMES_PER_SECOND: u64 = 8000;
/// Largest deviation from the nominal rate a feedback report may ask for,
/// as a divisor of the nominal rate (an eighth: far beyond any real clock
/// error, small enough that a hostile device cannot starve or flood).
const TOLERANCE: u64 = 8;

/// Microframes in one service interval of an isochronous endpoint with
/// this `bInterval` (full and low speed count 1 ms frames, faster speeds
/// microframes; both as `2^(bInterval-1)`).
pub fn esit_microframes(high_speed: bool, b_interval: u8) -> u32 {
    let exponent = u32::from(b_interval.clamp(1, 16) - 1);
    let units = 1u32 << exponent.min(15);
    if high_speed {
        units
    } else {
        units.saturating_mul(8)
    }
}

/// The pace of one playback stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pacer {
    rate: u32,
    /// Microframes per service interval.
    micro: u32,
    high_speed: bool,
    /// The most frames one service interval may carry (its payload limit).
    max_frames: u32,
    /// Remainder of the exact nominal accumulator, in frame * microframe
    /// units over [`MICROFRAMES_PER_SECOND`].
    exact: u64,
    /// The device's rate in frames per microframe, 16.16, once it reported.
    feedback: Option<u64>,
    /// Fractional frames of the feedback accumulator, 16.16.
    fraction: u64,
}

impl Pacer {
    pub fn new(rate: u32, micro: u32, max_frames: u32, high_speed: bool) -> Pacer {
        Pacer {
            rate,
            micro: micro.max(1),
            high_speed,
            max_frames,
            exact: 0,
            feedback: None,
            fraction: 0,
        }
    }

    /// The nominal rate in frames per microframe, 16.16.
    fn nominal(&self) -> u64 {
        (u64::from(self.rate) << 16) / MICROFRAMES_PER_SECOND
    }

    /// Frames for the next service interval.
    pub fn next_frames(&mut self) -> u32 {
        let frames = match self.feedback {
            Some(per_micro) => {
                self.fraction += per_micro * u64::from(self.micro);
                let whole = self.fraction >> 16;
                self.fraction &= 0xFFFF;
                whole
            }
            None => {
                self.exact += u64::from(self.rate) * u64::from(self.micro);
                let whole = self.exact / MICROFRAMES_PER_SECOND;
                self.exact %= MICROFRAMES_PER_SECOND;
                whole
            }
        };
        (frames as u32).min(self.max_frames)
    }

    /// Most frames any one interval can need at this pace (the size of a
    /// TD's buffer slot): the nominal count rounded up, plus one for the
    /// accumulator and an eighth for feedback.
    pub fn slot_frames(&self) -> u32 {
        let nominal =
            (u64::from(self.rate) * u64::from(self.micro)).div_ceil(MICROFRAMES_PER_SECOND) as u32;
        (nominal + nominal / TOLERANCE as u32 + 1).min(self.max_frames)
    }

    /// Take a feedback report; whether it was accepted.
    pub fn feedback(&mut self, report: &[u8]) -> bool {
        let raw = match report.len() {
            3 => u64::from(u32::from_le_bytes([report[0], report[1], report[2], 0])),
            n if n >= 4 => u64::from(u32::from_le_bytes([
                report[0], report[1], report[2], report[3],
            ])),
            _ => return false,
        };
        // 16.16 per microframe at high speed; 10.14 per frame at full
        // speed is (raw << 2) / 8 in 16.16 per microframe.
        let base = if self.high_speed { raw } else { raw >> 1 };
        let nominal = self.nominal();
        let lo = nominal - nominal / TOLERANCE;
        let hi = nominal + nominal / TOLERANCE;
        for shift in [0i32, -1, 1, -2, 2, -3, 3, -4, 4] {
            let candidate = if shift < 0 {
                base << -shift
            } else {
                base >> shift
            };
            if (lo..=hi).contains(&candidate) {
                self.feedback = Some(candidate);
                return true;
            }
        }
        false
    }

    /// The pace in frames per second, for the log.
    pub fn rate_hz(&self) -> u32 {
        match self.feedback {
            Some(per_micro) => ((per_micro * MICROFRAMES_PER_SECOND) >> 16) as u32,
            None => self.rate,
        }
    }
}
