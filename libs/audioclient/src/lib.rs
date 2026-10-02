//! The LazyOS audio client (docs/audio-plan.md stage A4, issue #451).
//!
//! Applications play sound through the system mixer, `audiod`, registered as
//! [`NAME`]. This crate is the one place that knows how: the typed calls of
//! `os.lazy.audio.v1` ([`Client`]) and `os.lazy.audio.mixer.v1`
//! ([`MixerControl`]), and above them a blocking [`PlaybackStream`] that
//! hides the shared ring entirely:
//!
//! ```ignore
//! let mut out = PlaybackStream::open(transport, Params::new(48_000, 2))?;
//! out.write(&samples)?;      // blocks while the ring is full
//! out.set_volume(32768)?;    // half volume, applied by the mixer
//! let played = out.finish()?; // drain, then close
//! ```
//!
//! It is `no_std` + `alloc` and knows nothing about syscalls: a [`Transport`]
//! delivers requests and creates shared rings. The native runtime implements
//! it in `user::audio`; a musl program would implement it over its own
//! Messenger bindings; the host tests implement it over the real mixer engine
//! (`libs/audiomix`), so the ring arithmetic here is checked against the very
//! code that enforces the contract on the other side.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod client;
mod playback;

#[cfg(test)]
mod tests;

pub use client::{Client, MixerControl};
pub use libmessenger::BufferDesc;
pub use messenger_generated::os_lazy_audio_mixer_v1 as control_wire;
pub use messenger_generated::os_lazy_audio_v1 as wire;
pub use playback::{Params, PlaybackStream};

/// The system mixer: the name applications resolve.
pub const NAME: &str = "os.lazy.audio";

/// The card driver, whose one stream belongs to the mixer.
pub const CARD_NAME: &str = "os.lazy.audio.card";

/// Unity gain in 16.16 fixed point; see `os.lazy.audio.v1` `SetVolume`.
pub const UNITY_GAIN: u32 = 1 << 16;

/// The scheduler clock the transports count in.
pub const TICK_HZ: u64 = 100;

/// The stream parameters a service granted.
pub use wire::StreamGrant as Grant;

/// What a service (card or mixer) accepts.
pub use wire::AudioInfo as Info;

/// Why a call failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The service refused, or the transport failed, with this (positive)
    /// errno.
    Errno(i64),
    /// A reply did not decode.
    Malformed,
    /// No progress within the stall limit: the ring never emptied, or a
    /// drain never finished.
    Stalled,
    /// The service granted parameters this API cannot use.
    Unsupported,
}

impl Error {
    /// The errno, when the failure carried one.
    pub fn errno(self) -> Option<i64> {
        match self {
            Error::Errno(code) => Some(code),
            _ => None,
        }
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Errno(code) => write!(f, "errno {code}"),
            Error::Malformed => f.write_str("malformed reply"),
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
    /// The whole ring as a transferred buffer.
    pub fn desc(self) -> BufferDesc {
        BufferDesc {
            handle: self.handle,
            offset: 0,
            len: self.len,
            flags: 0,
        }
    }
}

/// What a request carries outside its body: the parcel's `handles` and
/// `buffers`, as the method's `transfers (...)` clause in `audio.midl`
/// declares them. Build it with the generated `encode_*_transfers`; every
/// other request carries [`Transfers::NONE`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Transfers {
    pub handles: alloc::vec::Vec<u64>,
    pub buffers: alloc::vec::Vec<BufferDesc>,
}

impl Transfers {
    /// A request with nothing outside its body.
    pub const NONE: Transfers = Transfers {
        handles: alloc::vec::Vec::new(),
        buffers: alloc::vec::Vec::new(),
    };
}

impl From<(alloc::vec::Vec<u64>, alloc::vec::Vec<BufferDesc>)> for Transfers {
    /// The pair a generated `encode_*_transfers` returns.
    fn from((handles, buffers): (alloc::vec::Vec<u64>, alloc::vec::Vec<BufferDesc>)) -> Self {
        Transfers { handles, buffers }
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

/// How requests reach a service, and where rings come from.
pub trait Transport {
    type Ring: RingBuffer;

    /// Send one request and return the reply body. A service failure comes
    /// back as [`Error::Errno`] with the service's errno. `transfers` travel
    /// in the parcel's `handles` and `buffers`, in order. `deadline` is an
    /// absolute [`Transport::now`] tick.
    fn call(
        &self,
        interface: u64,
        method: u32,
        body: alloc::vec::Vec<u8>,
        transfers: Transfers,
        deadline: Option<u64>,
    ) -> Result<alloc::vec::Vec<u8>>;

    /// A new zero-filled shared ring of `bytes` bytes.
    fn create_ring(&self, bytes: usize) -> Result<Self::Ring>;

    /// The current tick ([`TICK_HZ`] per second).
    fn now(&self) -> u64;

    /// Sleep about one tick.
    fn sleep(&self);
}

impl<T: Transport + ?Sized> Transport for &T {
    type Ring = T::Ring;

    fn call(
        &self,
        interface: u64,
        method: u32,
        body: alloc::vec::Vec<u8>,
        transfers: Transfers,
        deadline: Option<u64>,
    ) -> Result<alloc::vec::Vec<u8>> {
        (**self).call(interface, method, body, transfers, deadline)
    }

    fn create_ring(&self, bytes: usize) -> Result<Self::Ring> {
        (**self).create_ring(bytes)
    }

    fn now(&self) -> u64 {
        (**self).now()
    }

    fn sleep(&self) {
        (**self).sleep()
    }
}
