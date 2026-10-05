//! One driver stream's volume and mute: the state behind `sndd`'s
//! `SetVolume` / `SetMute` (`os.lazy.audio.v1`, issue #452), kept here so it
//! is tested on the host.
//!
//! The driver copies each committed period into its own DMA staging slot and
//! then calls [`StreamVolume::stage`] on that copy, so a client rewriting its
//! ring cannot undo the volume. Only interleaved `S16Le` can be scaled; any
//! other granted format accepts unity alone. Mute stages silence while the
//! stream keeps consuming, so its position keeps moving. The cost is one
//! multiply and shift per sample, and nothing at unity.

use crate::gain::{self, Gain};

/// Why a `SetVolume` was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VolumeError {
    /// Above [`gain::MAX_Q16`] (`EINVAL`).
    OutOfRange,
    /// A gain other than unity on a format the driver cannot scale
    /// (`ENOTSUP`).
    Unsupported,
}

/// A stream's gain and mute flag; starts at unity, unmuted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamVolume {
    gain: Gain,
    muted: bool,
}

impl Default for StreamVolume {
    fn default() -> Self {
        StreamVolume::new()
    }
}

impl StreamVolume {
    /// Unity gain, not muted.
    pub const fn new() -> StreamVolume {
        StreamVolume {
            gain: Gain::UNITY,
            muted: false,
        }
    }

    /// `SetVolume(gain_q16)` for a stream whose granted format is `S16Le`
    /// when `s16le`. A refused request leaves the volume unchanged.
    pub fn set_volume(&mut self, gain_q16: u32, s16le: bool) -> Result<(), VolumeError> {
        let gain = Gain::new(gain_q16).ok_or(VolumeError::OutOfRange)?;
        if !gain.is_unity() && !s16le {
            return Err(VolumeError::Unsupported);
        }
        self.gain = gain;
        Ok(())
    }

    /// `SetMute(mute)`. The gain is kept, so unmuting restores it.
    pub fn set_mute(&mut self, mute: bool) {
        self.muted = mute;
    }

    /// The current gain.
    pub fn gain(&self) -> Gain {
        self.gain
    }

    /// Whether the stream is muted.
    pub fn muted(&self) -> bool {
        self.muted
    }

    /// Apply the volume to one staged period in place (`s16le` as for
    /// [`set_volume`](Self::set_volume)).
    pub fn stage(&self, staged: &mut [u8], s16le: bool) {
        if self.muted {
            staged.fill(0);
        } else if s16le {
            gain::scale_s16le(staged, self.gain);
        }
    }
}
