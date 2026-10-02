//! What the mixer grants a client: the driver's own snapping rules
//! (`virtio_snd::params::grant`) applied to a virtual card that takes `S16Le`
//! at every IDL rate, mono or stereo.
//!
//! Using the same policy as `sndd` means a client sees identical behaviour from
//! the mixer and from a card: an unsupported rate snaps to the closest, a
//! format the mixer cannot read snaps to `S16Le`, a channel count above two
//! snaps to two, and only a malformed request fails.

use virtio_snd::params::{self, Grant, ParamError, Request, RATES};
use virtio_snd::wire::{direction, format, PcmInfo};

/// The largest client ring the mixer maps: four periods of 16 KiB.
pub const MAX_RING_BYTES: u32 = 64 * 1024;

/// Most channels a client stream may have.
pub const MAX_CHANNELS: u8 = 2;

/// The virtual card the policy runs against.
pub fn info() -> PcmInfo {
    PcmInfo {
        formats: 1 << format::S16,
        rates: RATES.iter().fold(0, |mask, &(_, code)| mask | 1u64 << code),
        direction: direction::OUTPUT,
        channels_min: 1,
        channels_max: MAX_CHANNELS,
    }
}

/// The parameters a stream opened with `request` runs at.
pub fn grant(request: &Request) -> Result<Grant, ParamError> {
    params::grant(&info(), request, MAX_RING_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use virtio_snd::params::{audio_direction, audio_format};

    fn request(format: u32, rate_hz: u32, channels: u32, period_bytes: u32) -> Request {
        Request {
            direction: audio_direction::PLAYBACK,
            format,
            rate_hz,
            channels,
            period_bytes,
        }
    }

    #[test]
    fn every_idl_rate_is_granted_exactly() {
        for &(hz, _) in RATES.iter() {
            let grant = grant(&request(audio_format::S16_LE, hz, 2, 4096)).unwrap();
            assert_eq!(grant.rate_hz, hz);
        }
    }

    #[test]
    fn odd_requests_snap() {
        let grant = grant(&request(audio_format::FLOAT32, 47000, 6, 4099)).unwrap();
        assert_eq!(grant.format, audio_format::S16_LE);
        assert_eq!(grant.rate_hz, 48000);
        assert_eq!(grant.channels, 2);
        assert_eq!(grant.period_bytes % 4, 0);
        assert!(grant.buffer_bytes() <= MAX_RING_BYTES);
        let mono = super::grant(&request(audio_format::S16_LE, 22050, 1, 1 << 30)).unwrap();
        assert_eq!((mono.channels, mono.rate_hz), (1, 22050));
        assert_eq!(mono.buffer_bytes(), MAX_RING_BYTES);
    }

    #[test]
    fn malformed_requests_fail() {
        for bad in [
            request(9, 48000, 2, 4096),
            request(audio_format::S16_LE, 48000, 0, 4096),
            request(audio_format::S16_LE, 48000, 999, 4096),
            request(audio_format::S16_LE, 48000, 2, 0),
        ] {
            assert_eq!(grant(&bad), Err(ParamError::Invalid));
        }
        let capture = Request {
            direction: audio_direction::CAPTURE,
            ..request(audio_format::S16_LE, 48000, 2, 4096)
        };
        assert_eq!(grant(&capture), Err(ParamError::Unsupported));
    }
}
