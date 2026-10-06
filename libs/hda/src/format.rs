//! Stream formats (HDA specification section 3.7.1) and the converter's
//! supported rates and sizes (`param::PCM`).
//!
//! A format word is the same in the controller's `SDnFMT` and the converter's
//! `SET_STREAM_FORMAT`: base rate (48 or 44.1 kHz, bit 14), multiplier (bits
//! 13..11), divisor (bits 10..8), sample size (bits 6..4), channels - 1 (bits
//! 3..0).

/// The rates of `param::PCM` bits 0..=11, in Hz.
pub const RATES_HZ: [u32; 12] = [
    8000, 11025, 16000, 22050, 32000, 44100, 48000, 88200, 96000, 176400, 192000, 384000,
];

/// Sample sizes of `param::PCM` bits 16..=20, in bits.
pub const SIZES: [u32; 5] = [8, 16, 20, 24, 32];

/// Whether the converter's `pcm` answer supports `rate_hz`.
pub fn supports_rate(pcm: u32, rate_hz: u32) -> bool {
    RATES_HZ
        .iter()
        .position(|&hz| hz == rate_hz)
        .is_some_and(|bit| pcm >> bit & 1 == 1)
}

/// Whether the converter's `pcm` answer supports `bits`-bit samples.
pub fn supports_size(pcm: u32, bits: u32) -> bool {
    SIZES
        .iter()
        .position(|&size| size == bits)
        .is_some_and(|index| pcm >> (16 + index) & 1 == 1)
}

/// The format word for `rate_hz`, `bits` per sample and `channels`, or `None`
/// for something the format cannot express.
pub fn encode(rate_hz: u32, bits: u32, channels: u32) -> Option<u16> {
    if channels == 0 || channels > 16 {
        return None;
    }
    let size = SIZES.iter().position(|&size| size == bits)? as u16;
    for (base_bit, base) in [(0u16, 48000u32), (1, 44100)] {
        for mult in 1..=4u32 {
            for div in 1..=8u32 {
                if base * mult == rate_hz * div {
                    return Some(
                        base_bit << 14
                            | ((mult - 1) as u16) << 11
                            | ((div - 1) as u16) << 8
                            | size << 4
                            | (channels - 1) as u16,
                    );
                }
            }
        }
    }
    None
}
