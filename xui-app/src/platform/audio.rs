//! The `libs/audioclient` runtime for Linux-ABI (musl) programs
//! (docs/audio-plan.md A4).
//!
//! `audioclient` holds the audio API: the generated `os.lazy.audio.v1` client
//! and the `PlaybackStream` over a shared ring. The native runtime plugs it in
//! with `user::audio`; this is the same seam over this crate's `int 0x80`
//! helpers, so a static musl program (the LazyRAD player, an xui app) can play
//! sound through the system mixer like any native client:
//!
//! ```ignore
//! let audio = xui_app::platform::audio::try_connect().ok_or("no sound")?;
//! let mut out = xui_app::platform::audio::open(audio, Params::new(22_050, 2))?;
//! out.try_write(&samples)?;
//! ```
//!
//! The calls travel over the mixer's Messenger endpoint, resolved by name and
//! kept; rings are display shared buffers, which the mixer maps when the ring
//! is attached. Every call is bounded by the interface's deadline, so a
//! stalled mixer costs a UI thread an error rather than a frozen window.

use std::ptr;

use audioclient::{Errno, RingBuffer, RingRef};
use libmessenger::client::Connection;

use super::messenger::Observed;
use crate::sys::{self, errno};

pub use audioclient::{Client, MixerControl, Params, NAME, TICK_HZ, UNITY_GAIN};

/// The xui runtime of a playback stream: rings from the display buffer
/// syscalls, the scheduler clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct Runtime;

/// A playback stream on the xui runtime.
pub type PlaybackStream = audioclient::PlaybackStream<Observed, Runtime>;

/// The service `name`, resolved once; `None` when no such service runs (an
/// image without a sound card), so a caller can fall back without waiting.
pub fn try_connect_to(name: &str) -> Option<Client<Observed>> {
    let client = Client::from_connection(Connection::named(Observed::default(), name.to_owned()));
    client.connection().try_connect().ok()?;
    Some(client)
}

/// The system mixer, [`NAME`].
pub fn try_connect() -> Option<Client<Observed>> {
    try_connect_to(NAME)
}

/// The mixer's control panel (`os.lazy.audio.mixer.v1`, served on [`NAME`]);
/// `None` when no mixer runs.
pub fn mixer() -> Option<MixerControl<Observed>> {
    let panel =
        MixerControl::from_connection(Connection::named(Observed::default(), NAME.to_owned()));
    panel.connection().try_connect().ok()?;
    Some(panel)
}

/// Open a playback stream on `audio`.
pub fn open(audio: Client<Observed>, params: Params) -> audioclient::Result<PlaybackStream> {
    PlaybackStream::open(audio, Runtime, params)
}

impl audioclient::Runtime for Runtime {
    type Ring = SharedRing;

    fn create_ring(&self, bytes: usize) -> Result<SharedRing, Errno> {
        let (handle, va, _) = sys::buffer_create(bytes as u64).map_err(Errno::from_syscall)?;
        if va == 0 {
            let _ = sys::buffer_close(handle);
            return Err(Errno::new(errno::EINVAL));
        }
        Ok(SharedRing {
            handle,
            base: va as *mut u8,
            len: bytes,
        })
    }

    fn now(&self) -> u64 {
        sys::clock_ticks()
    }

    fn sleep(&self) {
        sys::sleep_millis(1000 / TICK_HZ);
    }
}

/// A shared buffer this task created and writes samples into; closed (and
/// unmapped) when dropped.
pub struct SharedRing {
    handle: u64,
    base: *mut u8,
    len: usize,
}

impl RingBuffer for SharedRing {
    fn share(&self) -> RingRef {
        RingRef {
            handle: self.handle,
            len: self.len as u64,
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn write(&mut self, offset: usize, bytes: &[u8]) {
        let fits = offset
            .checked_add(bytes.len())
            .is_some_and(|end| end <= self.len);
        assert!(fits, "ring write out of bounds");
        // SAFETY: `offset + bytes.len() <= len`, checked above, and `base` is
        // the `len`-byte mapping `buffer_create` returned, live until
        // `drop` closes it. The mixer only reads this memory, so a raw copy
        // (no reference into the mapping) is all that is needed.
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), self.base.add(offset), bytes.len()) };
    }
}

impl Drop for SharedRing {
    fn drop(&mut self) {
        let _ = sys::buffer_close(self.handle);
    }
}
