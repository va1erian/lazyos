//! Map an `os.lazy.audio.v1` stream request onto what a virtio-sound stream
//! supports.
//!
//! The IDL promises the closest supported parameters rather than a failure for
//! a merely unsupported rate or period, and `EINVAL` for a request that makes
//! no sense (unknown format, zero or absurd channel count, zero period). This
//! module is that policy, pure so it can be tested against every combination.

use crate::wire::{direction, format, rate, PcmInfo};

/// `Format` ordinals of `idl/audio.midl` (also the `AudioInfo.formats` bits).
pub mod audio_format {
    pub const S16_LE: u32 = 0;
    pub const S24_LE: u32 = 1;
    pub const S32_LE: u32 = 2;
    pub const FLOAT32: u32 = 3;
    pub const COUNT: u32 = 4;
}

/// `Direction` ordinals of `idl/audio.midl`.
pub mod audio_direction {
    pub const PLAYBACK: u32 = 0;
    pub const CAPTURE: u32 = 1;
}

/// The IDL's sample rates: `(Hz, AudioInfo.rates bit, virtio rate code)`.
pub const RATES: [(u32, u8); 11] = [
    (8000, rate::R8000),
    (11025, rate::R11025),
    (16000, rate::R16000),
    (22050, rate::R22050),
    (32000, rate::R32000),
    (44100, rate::R44100),
    (48000, rate::R48000),
    (88200, rate::R88200),
    (96000, rate::R96000),
    (176400, rate::R176400),
    (192000, rate::R192000),
];

/// Most channels a stream may carry (the IDL's `AudioInfo.channels` ceiling).
pub const MAX_CHANNELS: u32 = 8;
/// Periods in a stream's ring.
pub const PERIODS: u32 = 4;
/// Smallest period, in frames: below this the per-message overhead dominates.
const MIN_PERIOD_FRAMES: u32 = 64;

/// What a client asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub direction: u32,
    pub format: u32,
    pub rate_hz: u32,
    pub channels: u32,
    pub period_bytes: u32,
}

/// What the driver will run the stream at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grant {
    /// `Format` ordinal.
    pub format: u32,
    pub rate_hz: u32,
    pub channels: u32,
    pub period_bytes: u32,
    pub periods: u32,
    pub virtio_format: u8,
    pub virtio_rate: u8,
}

impl Grant {
    pub fn buffer_bytes(&self) -> u32 {
        self.period_bytes * self.periods
    }

    pub fn frame_bytes(&self) -> u32 {
        frame_bytes(self.format, self.channels).unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamError {
    /// The request itself is malformed (`EINVAL`).
    Invalid,
    /// The stream cannot do anything close to the request (`ENOTSUP`).
    Unsupported,
}

/// The virtio format code for a `Format` ordinal.
pub fn virtio_format(ordinal: u32) -> Option<u8> {
    match ordinal {
        audio_format::S16_LE => Some(format::S16),
        audio_format::S24_LE => Some(format::S24),
        audio_format::S32_LE => Some(format::S32),
        audio_format::FLOAT32 => Some(format::FLOAT),
        _ => None,
    }
}

/// Bytes per sample *container*: 24-bit audio travels in 32-bit words.
fn sample_bytes(ordinal: u32) -> Option<u32> {
    match ordinal {
        audio_format::S16_LE => Some(2),
        audio_format::S24_LE | audio_format::S32_LE | audio_format::FLOAT32 => Some(4),
        _ => None,
    }
}

/// Bytes in one frame, or `None` for an unknown format or a bad channel count.
pub fn frame_bytes(ordinal: u32, channels: u32) -> Option<u32> {
    if channels == 0 || channels > MAX_CHANNELS {
        return None;
    }
    sample_bytes(ordinal)?.checked_mul(channels)
}

/// The `AudioInfo.formats` bitmap for a stream.
pub fn format_bitmap(info: &PcmInfo) -> u32 {
    (0..audio_format::COUNT)
        .filter(|&ordinal| virtio_format(ordinal).is_some_and(|code| info.supports_format(code)))
        .fold(0, |mask, ordinal| mask | 1 << ordinal)
}

/// The `AudioInfo.rates` bitmap for a stream.
pub fn rate_bitmap(info: &PcmInfo) -> u32 {
    RATES
        .iter()
        .enumerate()
        .filter(|(_, &(_, code))| info.supports_rate(code))
        .fold(0, |mask, (bit, _)| mask | 1 << bit)
}

/// Pick the format: the requested one if supported, else `S16Le`, else the
/// first supported.
fn pick_format(info: &PcmInfo, wanted: u32) -> Option<u32> {
    let supported = |ordinal: u32| virtio_format(ordinal).is_some_and(|c| info.supports_format(c));
    if supported(wanted) {
        return Some(wanted);
    }
    if supported(audio_format::S16_LE) {
        return Some(audio_format::S16_LE);
    }
    (0..audio_format::COUNT).find(|&ordinal| supported(ordinal))
}

/// Pick the supported rate closest to `wanted`; ties take the lower rate.
fn pick_rate(info: &PcmInfo, wanted: u32) -> Option<(u32, u8)> {
    RATES
        .iter()
        .copied()
        .filter(|&(_, code)| info.supports_rate(code))
        .min_by_key(|&(hz, _)| (hz.abs_diff(wanted), hz))
}

/// Choose stream parameters for `request` on a stream described by `info`,
/// with a ring of at most `max_buffer_bytes`.
pub fn grant(
    info: &PcmInfo,
    request: &Request,
    max_buffer_bytes: u32,
) -> Result<Grant, ParamError> {
    let wanted_direction = match request.direction {
        audio_direction::PLAYBACK => direction::OUTPUT,
        audio_direction::CAPTURE => direction::INPUT,
        _ => return Err(ParamError::Invalid),
    };
    if request.channels == 0 || request.channels > MAX_CHANNELS || request.period_bytes == 0 {
        return Err(ParamError::Invalid);
    }
    if request.format >= audio_format::COUNT {
        return Err(ParamError::Invalid);
    }
    if info.direction != wanted_direction {
        return Err(ParamError::Unsupported);
    }
    let format = pick_format(info, request.format).ok_or(ParamError::Unsupported)?;
    let (rate_hz, virtio_rate) = pick_rate(info, request.rate_hz).ok_or(ParamError::Unsupported)?;
    let channels = request.channels.clamp(
        u32::from(info.channels_min).max(1),
        u32::from(info.channels_max).max(1),
    );
    let frame = frame_bytes(format, channels).ok_or(ParamError::Invalid)?;

    // A whole number of frames per period, within [64 frames, ring / PERIODS].
    let ceiling = max_buffer_bytes / PERIODS / frame * frame;
    let floor = MIN_PERIOD_FRAMES * frame;
    if ceiling < floor {
        return Err(ParamError::Unsupported);
    }
    let period_bytes = (request.period_bytes / frame * frame).clamp(floor, ceiling);
    Ok(Grant {
        format,
        rate_hz,
        channels,
        period_bytes,
        periods: PERIODS,
        virtio_format: virtio_format(format).ok_or(ParamError::Invalid)?,
        virtio_rate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(formats: &[u8], rates: &[u8], min: u8, max: u8) -> PcmInfo {
        PcmInfo {
            formats: formats.iter().fold(0, |m, &f| m | 1u64 << f),
            rates: rates.iter().fold(0, |m, &r| m | 1u64 << r),
            direction: direction::OUTPUT,
            channels_min: min,
            channels_max: max,
        }
    }

    fn request(format: u32, rate_hz: u32, channels: u32, period_bytes: u32) -> Request {
        Request {
            direction: audio_direction::PLAYBACK,
            format,
            rate_hz,
            channels,
            period_bytes,
        }
    }

    const QEMU: fn() -> PcmInfo = || {
        // What QEMU's virtio-sound advertises (S16 and friends, 8-192 kHz).
        output(
            &[format::S16, format::S24, format::S32, format::FLOAT],
            &[rate::R44100, rate::R48000, rate::R96000],
            1,
            2,
        )
    };

    #[test]
    fn exact_request_is_granted_unchanged() {
        let grant = grant(&QEMU(), &request(0, 48000, 2, 4096), 65536).expect("grant");
        assert_eq!(grant.format, audio_format::S16_LE);
        assert_eq!(grant.rate_hz, 48000);
        assert_eq!(grant.channels, 2);
        assert_eq!(grant.period_bytes, 4096);
        assert_eq!(grant.periods, 4);
        assert_eq!(grant.buffer_bytes(), 16384);
        assert_eq!(
            (grant.virtio_format, grant.virtio_rate),
            (format::S16, rate::R48000)
        );
    }

    #[test]
    fn unsupported_rate_snaps_to_the_closest() {
        let info = QEMU();
        assert_eq!(
            grant(&info, &request(0, 47000, 2, 4096), 65536)
                .unwrap()
                .rate_hz,
            48000
        );
        assert_eq!(
            grant(&info, &request(0, 8000, 2, 4096), 65536)
                .unwrap()
                .rate_hz,
            44100
        );
        assert_eq!(
            grant(&info, &request(0, 192000, 2, 4096), 65536)
                .unwrap()
                .rate_hz,
            96000
        );
        // 46050 is equidistant from 44100 and 48000 (1950 vs 1950): the lower wins.
        assert_eq!(
            grant(&info, &request(0, 46050, 2, 4096), 65536)
                .unwrap()
                .rate_hz,
            44100
        );
    }

    #[test]
    fn unsupported_format_falls_back_to_s16() {
        let info = output(&[format::S16], &[rate::R48000], 1, 2);
        let grant = grant(
            &info,
            &request(audio_format::FLOAT32, 48000, 2, 4096),
            65536,
        )
        .unwrap();
        assert_eq!(grant.format, audio_format::S16_LE);
        let only_float = output(&[format::FLOAT], &[rate::R48000], 1, 2);
        let grant = super::grant(&only_float, &request(0, 48000, 2, 4096), 65536).unwrap();
        assert_eq!(grant.format, audio_format::FLOAT32);
    }

    #[test]
    fn channels_are_clamped_to_the_stream() {
        let grant = grant(&QEMU(), &request(0, 48000, 6, 4096), 65536).unwrap();
        assert_eq!(grant.channels, 2);
        let mono_only = output(&[format::S16], &[rate::R48000], 1, 1);
        assert_eq!(
            super::grant(&mono_only, &request(0, 48000, 2, 4096), 65536)
                .unwrap()
                .channels,
            1
        );
    }

    #[test]
    fn period_is_frame_aligned_and_bounded() {
        let info = QEMU();
        // 4-byte frames: 4099 rounds down to 4096.
        assert_eq!(
            grant(&info, &request(0, 48000, 2, 4099), 65536)
                .unwrap()
                .period_bytes,
            4096
        );
        // Tiny requests are raised to 64 frames.
        assert_eq!(
            grant(&info, &request(0, 48000, 2, 4), 65536)
                .unwrap()
                .period_bytes,
            256
        );
        // Huge ones are cut to a quarter of the ring.
        assert_eq!(
            grant(&info, &request(0, 48000, 2, 1 << 30), 65536)
                .unwrap()
                .period_bytes,
            16384
        );
        // 24-bit audio uses 4-byte containers: 2 ch = 8-byte frames.
        assert_eq!(
            grant(&info, &request(audio_format::S24_LE, 48000, 2, 4100), 65536)
                .unwrap()
                .period_bytes,
            4096
        );
    }

    #[test]
    fn malformed_requests_are_invalid() {
        let info = QEMU();
        for bad in [
            request(9, 48000, 2, 4096),
            request(0, 48000, 0, 4096),
            request(0, 48000, 9, 4096),
            request(0, 48000, 2, 0),
            Request {
                direction: 7,
                ..request(0, 48000, 2, 4096)
            },
        ] {
            assert_eq!(grant(&info, &bad, 65536), Err(ParamError::Invalid));
        }
    }

    #[test]
    fn wrong_direction_or_empty_capabilities_are_unsupported() {
        let info = QEMU();
        let capture = Request {
            direction: audio_direction::CAPTURE,
            ..request(0, 48000, 2, 4096)
        };
        assert_eq!(grant(&info, &capture, 65536), Err(ParamError::Unsupported));
        let no_rates = output(&[format::S16], &[], 1, 2);
        assert_eq!(
            grant(&no_rates, &request(0, 48000, 2, 4096), 65536),
            Err(ParamError::Unsupported)
        );
        // A ring too small to hold four 64-frame periods.
        assert_eq!(
            grant(&info, &request(0, 48000, 2, 4096), 512),
            Err(ParamError::Unsupported)
        );
    }

    #[test]
    fn bitmaps_follow_the_idl_bit_numbers() {
        let info = QEMU();
        assert_eq!(format_bitmap(&info), 0b1111);
        // 44100 is IDL bit 5, 48000 bit 6, 96000 bit 8.
        assert_eq!(rate_bitmap(&info), 1 << 5 | 1 << 6 | 1 << 8);
        assert_eq!(format_bitmap(&output(&[format::S16], &[], 1, 1)), 1);
    }

    #[test]
    fn frame_bytes_rejects_nonsense() {
        assert_eq!(frame_bytes(0, 2), Some(4));
        assert_eq!(frame_bytes(3, 8), Some(32));
        assert_eq!(frame_bytes(4, 2), None);
        assert_eq!(frame_bytes(0, 0), None);
        assert_eq!(frame_bytes(0, 9), None);
    }
}
