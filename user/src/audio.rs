//! The native transport for `libs/audioclient` (docs/audio-plan.md A4).
//!
//! `audioclient` holds the audio API itself: typed `os.lazy.audio.v1` and
//! `os.lazy.audio.mixer.v1` calls and the blocking `PlaybackStream`. This
//! module only plugs the native runtime in: requests travel over a Messenger
//! endpoint resolved by name, and rings are display shared buffers.
//!
//! ```ignore
//! let audio = user::audio::connect_wait(audioclient::NAME, 500)?;
//! let mut out = PlaybackStream::open(&audio, Params::new(48_000, 2))?;
//! ```

use alloc::vec::Vec;
use core::ptr;

use audioclient::{Error, Result, RingBuffer, RingRef, Transport};
use libmessenger::{BufferDesc, Header, Parcel, VERSION};

use crate::messenger::services::error_field;
use crate::messenger::{registry, Endpoint, Error as MsgError};
use crate::sys;

pub use audioclient::{
    Client, MixerControl, Params, PlaybackStream, CARD_NAME, NAME, TICK_HZ, UNITY_GAIN,
};

/// `EIO`, for transport failures that carry no errno of their own.
const EIO: i64 = 5;

/// A Messenger failure as the audio API reports it: a kernel errno becomes
/// its positive value, an undecodable parcel [`Error::Malformed`].
pub fn error_of(error: MsgError) -> Error {
    match error {
        MsgError::Errno(code) => Error::Errno(-code),
        MsgError::Parcel(_) => Error::Malformed,
        _ => Error::Errno(EIO),
    }
}

/// An audio service reached over a Messenger endpoint.
pub struct Native {
    endpoint: Endpoint,
}

impl Native {
    /// Resolve `name` ([`NAME`] for the mixer, [`CARD_NAME`] for the card).
    pub fn connect(name: &str) -> Result<Native> {
        let endpoint = registry::resolve(name).map_err(error_of)?;
        Ok(Native { endpoint })
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        // The endpoint was obtained by name, so other holders may still use
        // the channel: release this task's handle rather than closing it.
        // Without this, `audiod` would leak a handle each time it reconnects
        // to a restarted driver.
        let _ = self.endpoint.release();
    }
}

/// Resolve `name`, retrying for up to `ticks` while the service starts.
pub fn connect_wait(name: &str, ticks: u64) -> Result<Native> {
    let deadline = sys::clock() + ticks;
    loop {
        match Native::connect(name) {
            Ok(native) => return Ok(native),
            Err(error) if sys::clock() >= deadline => return Err(error),
            Err(_) => nap(),
        }
    }
}

/// Sleep one PIT tick (`wait` doubles as a timer when there is no child).
fn nap() {
    let _ = sys::wait(sys::clock() + 1);
}

impl Transport for Native {
    type Ring = NativeRing;

    fn call(
        &self,
        interface: u64,
        method: u32,
        body: Vec<u8>,
        ring: Option<RingRef>,
        deadline: Option<u64>,
    ) -> Result<Vec<u8>> {
        let buffers = ring
            .map(|ring| {
                alloc::vec![BufferDesc {
                    handle: ring.handle,
                    offset: 0,
                    len: ring.len,
                    flags: 0,
                }]
            })
            .unwrap_or_default();
        let request = Parcel {
            header: Header {
                version: VERSION,
                flags: 0,
                interface_id: interface,
                method,
                txn_id: 0,
                reply_to: 0,
                deadline_ns: 0,
            },
            body,
            buffers,
            ..Parcel::default()
        };
        let reply = self.endpoint.call(&request, deadline).map_err(error_of)?;
        match error_field(&reply).map_err(error_of)? {
            Some(code) => Err(Error::Errno(code)),
            None => Ok(reply.body),
        }
    }

    fn create_ring(&self, bytes: usize) -> Result<NativeRing> {
        let (handle, va) =
            sys::display_create_buffer(bytes as u64).map_err(|code| Error::Errno(-code))?;
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
        nap();
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
        // the `len`-byte mapping `display_create_buffer` returned, live until
        // `drop` closes it. The service only reads this memory, so a raw copy
        // (no reference into the mapping) is all that is needed.
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), self.base.add(offset), bytes.len()) };
    }
}

impl Drop for NativeRing {
    fn drop(&mut self) {
        let _ = sys::display_close_buffer(self.handle);
    }
}
