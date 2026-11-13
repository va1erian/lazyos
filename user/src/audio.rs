//! The native runtime for `libs/audioclient` (docs/audio-plan.md A4).
//!
//! `audioclient` holds the audio API itself: the generated `os.lazy.audio.v1`
//! and `os.lazy.audio.mixer.v1` clients and the blocking `PlaybackStream`.
//! This module only plugs the native runtime in: the calls travel over
//! [`Native`] (a Messenger endpoint resolved by name), and rings are display
//! shared buffers ([`Runtime`]).
//!
//! ```ignore
//! let audio = user::audio::connect_wait(audioclient::NAME)?;
//! let mut out = user::audio::open(audio, Params::new(48_000, 2))?;
//! ```

use core::ptr;

use audioclient::{CallError, Errno, RingBuffer, RingRef};
use libmessenger::client::Connection;

use crate::messenger::client::Native;
use crate::sys;

pub use audioclient::{Client, MixerControl, Params, CARD_PREFIX, NAME, TICK_HZ, UNITY_GAIN};

/// The native runtime of a playback stream: rings from the display buffer
/// syscalls, the scheduler clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct Runtime;

/// A playback stream on the native runtime.
pub type PlaybackStream = audioclient::PlaybackStream<Native, Runtime>;

/// A client of the audio service `name` ([`NAME`] for the mixer,
/// [`CARD_PREFIX`] and an id for a card), waiting the runtime's connect bound
/// ([`crate::messenger::client::CONNECT_BOUND`]) while the service starts.
pub fn connect_wait(name: &str) -> Result<Client<Native>, CallError> {
    let client = Client::from_connection(Connection::named(Native, name.into()));
    client
        .connection()
        .connect()
        .map_err(CallError::Transport)?;
    Ok(client)
}

/// [`connect_wait`] with one attempt, for a caller that falls back without
/// sound.
pub fn connect_once(name: &str) -> Result<Client<Native>, CallError> {
    let client = Client::from_connection(Connection::named(Native, name.into()));
    client
        .connection()
        .try_connect()
        .map_err(CallError::Transport)?;
    Ok(client)
}

/// The mixer's control panel (`os.lazy.audio.mixer.v1`, served on the same
/// name as the streams), waiting for the mixer to register.
pub fn mixer_control() -> Result<MixerControl<Native>, CallError> {
    MixerControl::connect(Native)
}

/// Open a playback stream on `client`.
pub fn open(client: Client<Native>, params: Params) -> audioclient::Result<PlaybackStream> {
    PlaybackStream::open(client, Runtime, params)
}

impl audioclient::Runtime for Runtime {
    type Ring = NativeRing;

    fn create_ring(&self, bytes: usize) -> Result<NativeRing, Errno> {
        let (handle, va, _) = sys::buffer_create(bytes as u64).map_err(Errno::from_syscall)?;
        Ok(NativeRing {
            handle,
            base: va as *mut u8,
            len: bytes,
        })
    }

    fn now(&self) -> u64 {
        sys::clock()
    }

    fn sleep(&self) {
        sys::nap();
    }
}

/// A shared buffer this task created and writes samples into; closed (and
/// unmapped) when dropped.
pub struct NativeRing {
    handle: u64,
    base: *mut u8,
    len: usize,
}

impl RingBuffer for NativeRing {
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
        // `drop` closes it. The service only reads this memory, so a raw copy
        // (no reference into the mapping) is all that is needed.
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), self.base.add(offset), bytes.len()) };
    }
}

impl Drop for NativeRing {
    fn drop(&mut self) {
        let _ = sys::buffer_close(self.handle);
    }
}
