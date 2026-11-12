//! The clipboard client: the generated `wire::Client` over a transport that
//! allows nested calls, plus the large-payload and lazy-offer helpers
//! ([`Clipboard`]) that are this service's own meaning.

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::client::{CallError, Deadline, Errno, Handle, Transport};
use libmessenger::{flags, Buffer, Parcel};

use super::{wire, MAX_BUFFER, MAX_DATA};
use crate::messenger::client::Native;
use crate::sys;

/// The native transport with `ALLOW_NESTED` on every request.
///
/// Every client resolves the same service endpoint, and a paste by one task
/// can overlap an offer by another, so the kernel's per-channel cycle check
/// would otherwise refuse the second call with `EDEADLK`. The service answers
/// each request before servicing the next and only calls out on a different
/// channel (the owner's `Serialize`), so nesting cannot form a cycle here.
#[derive(Clone, Copy, Debug, Default)]
pub struct Nested;

impl Transport for Nested {
    fn connect_bound(&self) -> Deadline {
        Native.connect_bound()
    }

    fn call_flags(&self) -> u16 {
        flags::ALLOW_NESTED
    }

    fn resolve(&self, name: &str, until: Deadline) -> Result<Handle, Errno> {
        Native.resolve(name, until)
    }

    fn call(&self, handle: Handle, request: &Parcel, until: Deadline) -> Result<Parcel, Errno> {
        Native.call(handle, request, until)
    }

    fn send(&self, handle: Handle, request: &Parcel) -> Result<(), Errno> {
        Native.send(handle, request)
    }

    fn release(&self, handle: Handle) {
        Native.release(handle);
    }
}

/// A blocking client of the clipboard service: `Client::connect(Nested)`
/// (waits for `clipboardd` to register) or `try_connect` for one attempt.
pub type Client = wire::Client<Nested>;

/// Why a clipboard call failed (the interface declares no error codes: a
/// refusal is the service's errno-style code and sentence).
pub type ClientError = wire::ClientError;

/// The session id the caller's requests are stamped with (the kernel stamps
/// it; this reads the same value for a program that names it in a topic).
pub fn session() -> Result<u64, ClientError> {
    sys::cred_get(None)
        .map(|cred| cred.session)
        .map_err(|code| CallError::Transport(Errno::from_syscall(code)))
}

/// The helpers a clipboard user wants beyond one generated call.
pub trait Clipboard {
    /// `Offer`: publish typed payloads for this task's session (the eager
    /// path, bounded by [`MAX_DATA`]); one payload past it goes through
    /// [`Clipboard::copy_large`].
    fn copy(&self, owner: &str, offers: &[(&str, &[u8])]) -> Result<u64, ClientError>;

    /// `OfferBuffer`: publish one payload of up to [`MAX_BUFFER`] bytes. The
    /// bytes go into fresh shared pages the service snapshots during the call,
    /// so this task's own handle closes as soon as the call returns.
    fn copy_large(&self, owner: &str, mime: &str, bytes: &[u8]) -> Result<u64, ClientError>;

    /// `Offer` for a lazy offer: this task keeps the data and serves
    /// [`super::method::SERIALIZE`] on the endpoint it registers under `sink`.
    fn offer_lazy(&self, owner: &str, sink: &str, mimes: &[&str]) -> Result<u64, ClientError>;

    /// Paste the newest offer of this session that carries `mime`;
    /// `Ok(None)` when no offer has it.
    fn paste(&self, mime: &str) -> Result<Option<Vec<u8>>, ClientError>;

    /// Paste one exact offer by token (0: the newest); a foreign-session
    /// token is refused with `EACCES` and audited by the service. A payload
    /// past the inline cap is read through a shared buffer (`Fetch`).
    fn paste_token(&self, token: u64, mime: &str) -> Result<Vec<u8>, ClientError>;
}

fn refuse(errno: Errno) -> ClientError {
    CallError::Transport(errno)
}

fn syscall(code: i64) -> ClientError {
    refuse(Errno::from_syscall(code))
}

impl<T: Transport> Clipboard for wire::Client<T> {
    fn copy(&self, owner: &str, offers: &[(&str, &[u8])]) -> Result<u64, ClientError> {
        // One payload past the inline cap goes through a shared buffer.
        if let [(mime, bytes)] = offers {
            if bytes.len() > MAX_DATA {
                return self.copy_large(owner, mime, bytes);
            }
        }
        let mimes: Vec<String> = offers.iter().map(|(mime, _)| String::from(*mime)).collect();
        let data: Vec<wire::Payload> = offers
            .iter()
            .map(|(mime, bytes)| wire::Payload {
                mime: String::from(*mime),
                bytes: bytes.to_vec(),
            })
            .collect();
        self.offer(owner, None, &mimes, &data)
    }

    fn copy_large(&self, owner: &str, mime: &str, bytes: &[u8]) -> Result<u64, ClientError> {
        if bytes.is_empty() || bytes.len() > MAX_BUFFER {
            return Err(refuse(Errno::TOOBIG));
        }
        let (handle, va, _) = sys::buffer_create(bytes.len() as u64).map_err(syscall)?;
        // SAFETY: `buffer_create` mapped at least `bytes.len()` writable bytes
        // at `va`, owned by this task until the buffer is closed below.
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), va as *mut u8, bytes.len()) };
        let token = self.offer_buffer(owner, mime, Buffer::whole(handle, bytes.len() as u64));
        let _ = sys::buffer_close(handle);
        token
    }

    fn offer_lazy(&self, owner: &str, sink: &str, mimes: &[&str]) -> Result<u64, ClientError> {
        let mimes: Vec<String> = mimes.iter().map(|mime| String::from(*mime)).collect();
        self.offer(owner, Some(sink), &mimes, &[])
    }

    fn paste(&self, mime: &str) -> Result<Option<Vec<u8>>, ClientError> {
        match self.paste_token(0, mime) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.code() == -crate::messenger::errno::ENOENT => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn paste_token(&self, token: u64, mime: &str) -> Result<Vec<u8>, ClientError> {
        let found = self.request(token, mime)?;
        if found.size > MAX_BUFFER as u64 {
            return Err(refuse(Errno::TOOBIG));
        }
        if found.bytes.len() as u64 == found.size {
            return Ok(found.bytes);
        }
        fetch(self, found.token, mime, found.size as usize)
    }
}

/// `Fetch`: the `size` bytes of offer `token` through a buffer this task
/// creates and closes.
fn fetch<T: Transport>(
    client: &wire::Client<T>,
    token: u64,
    mime: &str,
    size: usize,
) -> Result<Vec<u8>, ClientError> {
    let (handle, va, _) = sys::buffer_create(size as u64).map_err(syscall)?;
    let written = client.fetch(token, mime, Buffer::whole(handle, size as u64));
    let result = match written {
        Ok(written) if written as usize <= size => {
            // SAFETY: `buffer_create` mapped `size` readable bytes at `va`,
            // and `written <= size` is checked before reading.
            let bytes =
                unsafe { core::slice::from_raw_parts(va as *const u8, written as usize) }.to_vec();
            Ok(bytes)
        }
        Ok(_) => Err(refuse(Errno::INVAL)),
        Err(error) => Err(error),
    };
    let _ = sys::buffer_close(handle);
    result
}
