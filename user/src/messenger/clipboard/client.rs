//! [`Client`]: a client of the clipboard service.

use alloc::vec::Vec;

use libmessenger::{Buffer, Parcel};

use super::super::{errno, registry, Endpoint, Error, Result};
use super::protocol::{
    current_request, decode_current, decode_request_reply, decode_token, decode_written,
    error_field, fetch_request, offer_buffer_request, offer_lazy_request, offer_request,
    ping_request, request_request,
};
use super::{OfferInfo, MAX_BUFFER, MAX_DATA, NAME};
use crate::sys;

/// A client of the clipboard service.
pub struct Client {
    endpoint: Endpoint,
    session: u64,
}

impl Client {
    /// Resolve [`NAME`] and read the caller's session from the kernel.
    pub fn connect() -> Result<Client> {
        Client::from_endpoint(registry::resolve(NAME)?)
    }

    /// Wrap an already-resolved endpoint.
    pub fn from_endpoint(endpoint: Endpoint) -> Result<Client> {
        let cred = sys::cred_get(None).map_err(Error::Errno)?;
        Ok(Client {
            endpoint,
            session: cred.session,
        })
    }

    /// The underlying service endpoint (diagnostics).
    pub fn endpoint(&self) -> Endpoint {
        self.endpoint
    }

    /// The session id the client's requests are stamped with.
    pub fn session(&self) -> u64 {
        self.session
    }

    /// Run one request as a blocking call and fail on a service error
    /// reply.
    fn call(&self, request: &Parcel) -> Result<Parcel> {
        let reply = self.endpoint.call(request, None)?;
        if let Some(code) = error_field(&reply)? {
            return Err(Error::Errno(-code));
        }
        Ok(reply)
    }

    /// `Offer(owner, mime_types) -> token`: publish the typed payloads for
    /// this task's session (the eager path, bounded by the service's
    /// [`super::MAX_DATA`]). [`Client::offer_lazy`] is the on-demand variant.
    pub fn copy(&self, owner: &str, offers: &[(&str, &[u8])]) -> Result<u64> {
        // One payload past the inline cap goes through a shared buffer.
        if let [(mime, bytes)] = offers {
            if bytes.len() > MAX_DATA {
                return self.copy_large(owner, mime, bytes);
            }
        }
        let reply = self.call(&offer_request(owner, offers)?)?;
        decode_token(&reply)
    }

    /// `OfferBuffer`: publish one payload of up to [`MAX_BUFFER`] bytes. The
    /// bytes go into fresh shared pages the service snapshots during the call,
    /// so this task's own handle closes as soon as the call returns.
    pub fn copy_large(&self, owner: &str, mime: &str, bytes: &[u8]) -> Result<u64> {
        if bytes.is_empty() || bytes.len() > MAX_BUFFER {
            return Err(Error::Errno(-errno::E2BIG));
        }
        let (handle, va, _) = sys::buffer_create(bytes.len() as u64).map_err(Error::Errno)?;
        // SAFETY: `buffer_create` mapped at least `bytes.len()` writable bytes
        // at `va`, owned by this task until the buffer is closed below.
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), va as *mut u8, bytes.len()) };
        let request = offer_buffer_request(owner, mime, Buffer::whole(handle, bytes.len() as u64));
        let reply = request.and_then(|request| self.call(&request));
        let _ = sys::buffer_close(handle);
        decode_token(&reply?)
    }

    /// `Offer(owner, mime_types) -> token` for a lazy offer: this task
    /// keeps the data and serves [`super::method::SERIALIZE`] on the endpoint it
    /// registers under `sink`.
    pub fn offer_lazy(&self, owner: &str, sink: &str, mimes: &[&str]) -> Result<u64> {
        let reply = self.call(&offer_lazy_request(owner, sink, mimes)?)?;
        decode_token(&reply)
    }

    /// Paste the newest offer of this session that carries `mime`;
    /// `Ok(None)` when no offer has it.
    pub fn paste(&self, mime: &str) -> Result<Option<Vec<u8>>> {
        match self.paste_token(0, mime) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(Error::Errno(code)) if code == -errno::ENOENT => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Paste one exact offer by token (0: the newest); a foreign-session
    /// token is refused with `-EACCES` and audited by the service. A payload
    /// past the inline cap is read through a shared buffer (`Fetch`).
    pub fn paste_token(&self, token: u64, mime: &str) -> Result<Vec<u8>> {
        let reply = self.call(&request_request(token, mime)?)?;
        let found = decode_request_reply(&reply)?;
        if found.size > MAX_BUFFER as u64 {
            return Err(Error::Errno(-errno::E2BIG));
        }
        if found.bytes.len() as u64 == found.size {
            return Ok(found.bytes);
        }
        self.fetch(found.token, mime, found.size as usize)
    }

    /// `Fetch`: the `size` bytes of offer `token` through a buffer this task
    /// creates and closes.
    fn fetch(&self, token: u64, mime: &str, size: usize) -> Result<Vec<u8>> {
        let (handle, va, _) = sys::buffer_create(size as u64).map_err(Error::Errno)?;
        let request = fetch_request(token, mime, Buffer::whole(handle, size as u64));
        let reply = request.and_then(|request| self.call(&request));
        let written = reply.and_then(|reply| decode_written(&reply));
        let result = match written {
            // SAFETY: `buffer_create` mapped `size` readable bytes at `va`,
            // and `written <= size` is checked before reading.
            Ok(written) if written as usize <= size => {
                Ok(unsafe { core::slice::from_raw_parts(va as *const u8, written as usize) }.to_vec())
            }
            Ok(_) => Err(Error::Errno(-errno::EINVAL)),
            Err(error) => Err(error),
        };
        let _ = sys::buffer_close(handle);
        result
    }

    /// The current offer's metadata (never content).
    pub fn current(&self) -> Result<Option<OfferInfo>> {
        let reply = self.call(&current_request())?;
        decode_current(&reply)
    }

    /// Round-trip probe.
    pub fn ping(&self) -> Result<()> {
        self.call(&ping_request())?;
        Ok(())
    }
}
