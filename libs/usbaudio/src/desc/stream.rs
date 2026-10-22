//! AudioStreaming alternates (UAC 1.0 4.5-4.6, UAC 2.0 4.9-4.10; the
//! Frmts 1.0/2.0 type I format records): what one alternate setting carries
//! and through which endpoints.

use alloc::vec::Vec;

use super::{u16_at, u24_at, u32_at, Version, MAX_RANGES};

/// AudioStreaming class-specific subtypes.
const AS_GENERAL: u8 = 1;
const FORMAT_TYPE: u8 = 2;
/// Format type I (PCM-like, one sample per subslot per channel).
const FORMAT_TYPE_I: u8 = 1;
/// UAC 1.0 `wFormatTag` PCM, UAC 2.0 `bmFormats` bit 0 PCM.
const PCM: u32 = 1;
/// Endpoints kept per alternate (data and feedback; a hostile list stops).
const MAX_ENDPOINTS: usize = 4;
/// Channels a stream may have (7.1 at most).
pub const MAX_CHANNELS: u8 = 8;

/// An endpoint's synchronization type (`bmAttributes` bits 2..=3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sync {
    None,
    Async,
    Adaptive,
    Synchronous,
}

/// An isochronous endpoint descriptor (the audio class's 9-byte form, or
/// the standard 7-byte one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IsoEndpoint {
    pub address: u8,
    pub attributes: u8,
    /// Bytes per transaction (`wMaxPacketSize` bits 0..=10).
    pub max_packet: u16,
    /// Transactions per microframe at high speed (1..=3, bits 11..=12 + 1).
    pub transactions: u8,
    /// `bInterval`, in the encoding of the device's speed.
    pub interval: u8,
    /// UAC 1.0 feedback endpoints: `bRefresh` (2^n frames between reports).
    pub refresh: u8,
    /// UAC 1.0 data endpoints: the feedback endpoint's address, 0 for none.
    pub synch_address: u8,
}

impl IsoEndpoint {
    fn parse(record: &[u8]) -> Option<IsoEndpoint> {
        if record.len() < 7 || record[3] & 0b11 != 1 || record[2] & 0x0F == 0 {
            return None;
        }
        let size = u16_at(record, 4);
        let (refresh, synch_address) = if record.len() >= 9 {
            (record[7], record[8])
        } else {
            (0, 0)
        };
        let max_packet = size & 0x07FF;
        (max_packet != 0).then_some(IsoEndpoint {
            address: record[2],
            attributes: record[3],
            max_packet,
            transactions: ((size >> 11) & 0b11).min(2) as u8 + 1,
            interval: record[6],
            refresh,
            synch_address,
        })
    }

    pub fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }

    pub fn sync(&self) -> Sync {
        match (self.attributes >> 2) & 0b11 {
            0 => Sync::None,
            1 => Sync::Async,
            2 => Sync::Adaptive,
            _ => Sync::Synchronous,
        }
    }

    /// Usage type (bits 4..=5) says explicit feedback.
    pub fn is_feedback(&self) -> bool {
        (self.attributes >> 4) & 0b11 == 1
    }

    /// Most bytes in one service interval.
    pub fn max_esit(&self) -> u32 {
        u32::from(self.max_packet) * u32::from(self.transactions)
    }
}

/// One rate range: `min..=max` in steps of `step`; a single rate has
/// `min == max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub min: u32,
    pub max: u32,
    /// 0: any rate in between (a continuous range) or none (a single one).
    pub step: u32,
}

/// The rates an alternate (or a UAC 2.0 clock) runs at. Empty for a UAC
/// 2.0 alternate until its clock was asked (`Function::set_clock_rates`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RateSet {
    pub ranges: Vec<Range>,
}

impl RateSet {
    pub fn contains(&self, rate: u32) -> bool {
        self.ranges.iter().any(|range| {
            rate >= range.min
                && rate <= range.max
                && (range.step == 0 || (rate - range.min).is_multiple_of(range.step))
        })
    }

    /// Add a range, up to [`MAX_RANGES`]; a range with `min > max` or a
    /// zero rate is not a rate and is dropped.
    pub fn push(&mut self, range: Range) {
        if range.min != 0 && range.min <= range.max && self.ranges.len() < MAX_RANGES {
            self.ranges.push(range);
        }
    }

    /// No rate at all (a UAC 2.0 alternate whose clock was not asked yet).
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }
}

/// A PCM type I format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub channels: u8,
    /// Bytes per sample in the stream (2, 3 or 4).
    pub subslot: u8,
    /// Significant bits per sample.
    pub bits: u8,
}

impl Format {
    pub fn frame_bytes(&self) -> u32 {
        u32::from(self.channels) * u32::from(self.subslot)
    }

    fn valid(&self) -> bool {
        (1..=MAX_CHANNELS).contains(&self.channels)
            && (2..=4).contains(&self.subslot)
            && self.bits >= 8
            && u32::from(self.bits) <= u32::from(self.subslot) * 8
    }
}

/// A playback alternate setting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alt {
    pub interface: u8,
    pub alternate: u8,
    /// The input terminal the stream enters through (`bTerminalLink`).
    pub terminal: u8,
    pub format: Format,
    pub rates: RateSet,
    /// The isochronous OUT endpoint carrying the samples.
    pub data: IsoEndpoint,
    /// The explicit feedback IN endpoint of an asynchronous stream.
    pub feedback: Option<IsoEndpoint>,
    /// UAC 1.0: the data endpoint has a Sampling Frequency control.
    pub rate_control: bool,
}

/// An alternate being read, record by record.
pub(super) struct Builder {
    interface: u8,
    alternate: u8,
    terminal: Option<u8>,
    pcm: bool,
    format: Option<Format>,
    rates: RateSet,
    endpoints: Vec<IsoEndpoint>,
    rate_control: bool,
}

impl Builder {
    pub(super) fn new(interface: u8, alternate: u8) -> Builder {
        Builder {
            interface,
            alternate,
            terminal: None,
            pcm: false,
            format: None,
            rates: RateSet::default(),
            endpoints: Vec::new(),
            rate_control: false,
        }
    }

    pub(super) fn class_record(&mut self, version: Version, record: &[u8]) {
        if record.len() < 4 {
            return;
        }
        match (record[2], version) {
            (AS_GENERAL, Version::Uac1) if record.len() >= 7 => {
                self.terminal = Some(record[3]);
                self.pcm = u32::from(u16_at(record, 5)) == PCM;
            }
            (AS_GENERAL, Version::Uac2) if record.len() >= 11 => {
                self.terminal = Some(record[3]);
                self.pcm = record[5] == FORMAT_TYPE_I && u32_at(record, 6) & PCM != 0;
                // The channel count lives here in 2.0 (the format record
                // has only the subslot); kept until the format arrives.
                self.format = Some(Format {
                    channels: record[10],
                    subslot: self.format.map_or(0, |f| f.subslot),
                    bits: self.format.map_or(0, |f| f.bits),
                });
            }
            (FORMAT_TYPE, Version::Uac1) if record.len() >= 8 && record[3] == FORMAT_TYPE_I => {
                self.format = Some(Format {
                    channels: record[4],
                    subslot: record[5],
                    bits: record[6],
                });
                self.rates = uac1_rates(record);
            }
            (FORMAT_TYPE, Version::Uac2) if record.len() >= 6 && record[3] == FORMAT_TYPE_I => {
                self.format = Some(Format {
                    channels: self.format.map_or(0, |f| f.channels),
                    subslot: record[4],
                    bits: record[5],
                });
            }
            _ => {}
        }
    }

    pub(super) fn endpoint(&mut self, record: &[u8]) {
        if self.endpoints.len() < MAX_ENDPOINTS {
            if let Some(endpoint) = IsoEndpoint::parse(record) {
                self.endpoints.push(endpoint);
            }
        }
    }

    /// The class-specific endpoint record after an endpoint (UAC 1.0: bit 0
    /// of `bmAttributes` is the Sampling Frequency control).
    pub(super) fn class_endpoint(&mut self, record: &[u8]) {
        if record.len() >= 4 && record[2] == 1 && self.endpoints.len() == 1 {
            self.rate_control = record[3] & 1 != 0;
        }
    }

    /// The alternate, if it is PCM type I playback with a usable format.
    pub(super) fn build(self) -> Option<Alt> {
        let format = self.format.filter(Format::valid)?;
        if !self.pcm {
            return None;
        }
        let data = *self
            .endpoints
            .iter()
            .find(|endpoint| !endpoint.is_in() && !endpoint.is_feedback())?;
        // The feedback endpoint: the one the data endpoint names (1.0),
        // else the alternate's IN endpoint marked or used as feedback.
        let feedback = (data.sync() == Sync::Async)
            .then(|| {
                self.endpoints
                    .iter()
                    .filter(|endpoint| endpoint.is_in())
                    .find(|endpoint| {
                        (data.synch_address != 0 && endpoint.address == data.synch_address | 0x80)
                            || endpoint.is_feedback()
                    })
                    .or_else(|| self.endpoints.iter().find(|endpoint| endpoint.is_in()))
                    .copied()
            })
            .flatten();
        Some(Alt {
            interface: self.interface,
            alternate: self.alternate,
            terminal: self.terminal?,
            format,
            rates: self.rates,
            data,
            feedback,
            rate_control: self.rate_control,
        })
    }
}

/// The rates of a UAC 1.0 type I format record: `bSamFreqType` 0 is a
/// continuous range, otherwise that many discrete 3-byte rates.
fn uac1_rates(record: &[u8]) -> RateSet {
    let mut rates = RateSet::default();
    let count = usize::from(record[7]);
    if count == 0 {
        if record.len() >= 14 {
            rates.push(Range {
                min: u24_at(record, 8),
                max: u24_at(record, 11),
                step: 0,
            });
        }
        return rates;
    }
    for index in 0..count.min(MAX_RANGES) {
        let at = 8 + index * 3;
        if at + 3 > record.len() {
            break;
        }
        let rate = u24_at(record, at);
        rates.push(Range {
            min: rate,
            max: rate,
            step: 0,
        });
    }
    rates
}
