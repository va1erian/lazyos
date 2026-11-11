//! The `libs/audioclient` transport for Linux-ABI (musl) programs
//! (docs/audio-plan.md A4).
//!
//! `audioclient` holds the audio API: typed `os.lazy.audio.v1` calls and the
//! `PlaybackStream` over a shared ring. The native runtime plugs it in with
//! `user::audio`; this is the same seam over this crate's `int 0x80` helpers,
//! so a static musl program (the LazyRAD player, an xui app) can play sound
//! through the system mixer like any native client:
//!
//! ```ignore
//! let audio = xui_app::platform::audio::Audio::try_connect().ok_or("no sound")?;
//! let mut out = PlaybackStream::open(audio, Params::new(22_050, 2))?;
//! out.try_write(&samples)?;
//! ```
//!
//! Requests travel over the mixer's Messenger endpoint, resolved by name;
//! rings are display shared buffers, which the mixer maps when the ring is
//! attached. Every call is bounded (`CALL_TICKS`), so a stalled mixer costs a
//! UI thread an error rather than a frozen window.

use std::ptr;
use std::rc::Rc;

use audioclient::{Error, Object, Result, RingBuffer, RingRef, Transport};
use libmessenger::client::{CallError, Deadline};

use super::messenger::{shared, Conn};
use crate::sys::{self, errno};

pub use audioclient::{Params, PlaybackStream, NAME, TICK_HZ, UNITY_GAIN};

/// The longest a call may take when the caller set no deadline (2 s).
const CALL_TICKS: u64 = 2 * TICK_HZ;

/// A failed syscall (a negative errno) as the audio API reports it.
fn errno_of(code: i64) -> Error {
    Error::Errno(code.unsigned_abs() as i64)
}

/// The system mixer (or a card) reached over this thread's shared
/// connection to it ([`super::messenger::shared`]): resolved once per name
/// and reused; a call that finds the service gone drops the handle and
/// resolves a restarted mixer once.
pub struct Audio {
    conn: Rc<Conn>,
}

impl Audio {
    /// The service `name`, resolved once; `None` when no such service runs (an
    /// image without a sound card), so a caller can fall back without waiting.
    pub fn try_connect_to(name: &'static str) -> Option<Audio> {
        let conn = shared(name);
        conn.try_connect().ok()?;
        Some(Audio { conn })
    }

    /// The system mixer, [`NAME`].
    pub fn try_connect() -> Option<Audio> {
        Audio::try_connect_to(NAME)
    }
}

impl Transport for Audio {
    type Ring = SharedRing;

    fn call(
        &self,
        interface: u64,
        method: u32,
        body: Vec<u8>,
        objects: Vec<Object>,
        deadline: Option<u64>,
    ) -> Result<Vec<u8>> {
        let until = match deadline {
            Some(absolute) => Deadline::ticks(absolute.saturating_sub(sys::clock_ticks()).max(1)),
            None => Deadline::ticks(CALL_TICKS),
        };
        match self.conn.call(interface, method, body, objects, until) {
            Ok(reply) => Ok(reply.body),
            Err(CallError::Remote { code, .. }) => Err(Error::Errno(i64::from(code))),
            Err(error) => Err(errno_of(error.code())),
        }
    }

    fn create_ring(&self, bytes: usize) -> Result<SharedRing> {
        let (handle, va, _) = sys::buffer_create(bytes as u64).map_err(errno_of)?;
        if va == 0 {
            let _ = sys::buffer_close(handle);
            return Err(Error::Errno(errno::EINVAL));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syscall_errnos_become_positive() {
        assert_eq!(errno_of(-11), Error::Errno(11));
    }
}
