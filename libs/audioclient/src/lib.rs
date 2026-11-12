//! The LazyOS audio client (docs/audio-plan.md stage A4, issue #451).
//!
//! Applications play sound through the system mixer, `audiod`, registered as
//! [`NAME`]. This crate is the one place that knows how: the typed calls are
//! the generated clients of `os.lazy.audio.v1` ([`Client`]) and
//! `os.lazy.audio.mixer.v1` ([`MixerControl`]) over any `libmessenger`
//! transport, and above them a blocking [`PlaybackStream`] that hides the
//! shared ring entirely:
//!
//! ```ignore
//! let mut out = PlaybackStream::open(client, runtime, Params::new(48_000, 2))?;
//! out.write(&samples)?;      // blocks while the ring is full
//! out.set_volume(32768)?;    // half volume, applied by the mixer
//! let played = out.finish()?; // drain, then close
//! ```
//!
//! It is `no_std` + `alloc` and knows nothing about syscalls or framing: a
//! [`Transport`] delivers the generated client's calls, and a [`Runtime`]
//! creates shared rings and tells the time. The native runtime implements
//! them in `user::audio`, xui apps in `xui_app::platform::audio`; the host
//! tests run the stream over the real mixer engine (`libs/audiomix`), so the
//! ring arithmetic here is checked against the very code that enforces the
//! contract on the other side.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod playback;

#[cfg(test)]
mod tests;

pub use libmessenger::client::{CallError, Deadline, Errno, Transport};
pub use libmessenger::{Buffer, Object};
pub use messenger_generated::os_lazy_audio_mixer_v1 as control_wire;
pub use messenger_generated::os_lazy_audio_v1 as wire;
pub use playback::{Params, PlaybackStream};

/// The generated client of `os.lazy.audio.v1`, against the mixer or a card.
pub type Client<T> = wire::Client<T>;

/// The generated client of `os.lazy.audio.mixer.v1`: the volume control panel.
pub type MixerControl<T> = control_wire::Client<T>;

/// The system mixer: the name applications resolve.
pub const NAME: &str = wire::SERVICE_NAME;

/// Where card drivers register, one name per card
/// (`os.lazy.audio.card/intel-hda0`, `.../usb-audio0`); each card's one
/// stream belongs to the mixer, which picks the card to play through.
pub const CARD_PREFIX: &str = wire::SERVICE_INSTANCE_PREFIX;

/// Unity gain in 16.16 fixed point; see `os.lazy.audio.v1` `SetVolume`.
pub const UNITY_GAIN: u32 = 1 << 16;

/// The scheduler clock the transports count in.
pub const TICK_HZ: u64 = 100;

/// The stream parameters a service granted.
pub use wire::StreamGrant as Grant;

/// What a service (card or mixer) accepts.
pub use wire::AudioInfo as Info;

/// Why a stream operation failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// A call failed: the transport's failure, the service's refusal, or a
    /// reply that did not decode.
    Call(CallError),
    /// No progress within the stall limit: the ring never emptied, or a
    /// drain never finished.
    Stalled,
    /// The service granted parameters this API cannot use.
    Unsupported,
}

impl Error {
    /// The (positive) errno, when the failure carried one.
    pub fn errno(&self) -> Option<i64> {
        match self {
            Error::Call(error) => Some(-error.code()),
            _ => None,
        }
    }

    /// A failure of the runtime (a ring that could not be made) with `errno`.
    pub fn transport(errno: Errno) -> Error {
        Error::Call(CallError::Transport(errno))
    }
}

impl From<CallError> for Error {
    fn from(error: CallError) -> Error {
        Error::Call(error)
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Call(error) => error.fmt(f),
            Error::Stalled => f.write_str("no progress (stalled)"),
            Error::Unsupported => f.write_str("granted parameters are unusable"),
        }
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// A shared ring as named in a request: the transport's buffer handle and the
/// byte length the service may map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingRef {
    pub handle: u64,
    pub len: u64,
}

impl RingRef {
    /// The whole ring as a `Buffer` field.
    pub fn buffer(self) -> Buffer {
        Buffer::whole(self.handle, self.len)
    }
}

/// A shared buffer this task writes samples into.
pub trait RingBuffer {
    /// How a request names it.
    fn share(&self) -> RingRef;

    /// Bytes in the ring.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Copy `bytes` to `offset`. Callers keep `offset + bytes.len() <=
    /// len()`; an implementation must check it anyway.
    fn write(&mut self, offset: usize, bytes: &[u8]);
}

/// What a runtime provides besides the connection (a [`Transport`]): shared
/// rings, the clock and a sleep.
pub trait Runtime {
    type Ring: RingBuffer;

    /// A new zero-filled shared ring of `bytes` bytes.
    fn create_ring(&self, bytes: usize) -> core::result::Result<Self::Ring, Errno>;

    /// The current tick ([`TICK_HZ`] per second).
    fn now(&self) -> u64;

    /// Sleep about one tick.
    fn sleep(&self);
}
