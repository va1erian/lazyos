//! [`Client`]: a client of the clipboard service.

use alloc::vec::Vec;

use libmessenger::Parcel;

use super::super::{errno, registry, Endpoint, Error, Result};
use super::protocol::{
    current_request, decode_bytes, decode_current, decode_token, error_field, offer_lazy_request,
    offer_request, ping_request, request_request,
};
use super::{OfferInfo, NAME};
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
        let mut cred = sys::Cred::default();
        sys::cred_get(None, &mut cred).map_err(Error::Errno)?;
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
        let reply = self.call(&offer_request(owner, offers)?)?;
        decode_token(&reply)
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
        match self.call(&request_request(0, mime)?) {
            Ok(reply) => Ok(Some(decode_bytes(&reply)?)),
            Err(Error::Errno(code)) if code == -errno::ENOENT => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Paste one exact offer by token; a foreign-session token is refused
    /// with `-EACCES` and audited by the service.
    pub fn paste_token(&self, token: u64, mime: &str) -> Result<Vec<u8>> {
        let reply = self.call(&request_request(token, mime)?)?;
        decode_bytes(&reply)
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
