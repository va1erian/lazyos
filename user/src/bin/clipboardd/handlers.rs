//! Clipboard request handlers: offers, pastes, lazy serialize and publishing.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use messenger_generated::topics;
use user::central;
use user::messenger::{clipboard as wire, errno, registry, Endpoint, Error};
use user::sys;

use super::state::{Clipboard, Offer};
use super::SERIALIZE_DEADLINE;

/// Where a `Request` found its payload.
enum Reading {
    /// An inline payload the service already holds.
    Eager { mime: String, bytes: Vec<u8> },
    /// A lazy offer: serialize through the owner endpoint.
    Lazy {
        mime: String,
        sink: String,
        cached: Option<Endpoint>,
    },
}

impl Clipboard {
    /// Record an offer for `session` and announce it on the changed topic.
    pub(super) fn offer(
        &mut self,
        request: wire::OfferRequest,
        session: u64,
        owner_slot: u64,
        tick: u64,
    ) -> Result<u64, Error> {
        let mimes: Vec<String> = request
            .mimes
            .into_iter()
            .filter(|mime| !mime.is_empty())
            .collect();
        if mimes.is_empty() || mimes.len() > wire::MAX_MIMES {
            return Err(Error::Errno(-errno::EINVAL));
        }
        if mimes.iter().any(|mime| mime.len() > wire::MAX_MIME) {
            return Err(Error::Errno(-errno::E2BIG));
        }
        if request.owner.len() > wire::MAX_TEXT {
            return Err(Error::Errno(-errno::E2BIG));
        }
        if let Some(sink) = &request.sink {
            if sink.len() > wire::MAX_TEXT {
                return Err(Error::Errno(-errno::E2BIG));
            }
        }
        let bytes: usize = request.data.iter().map(|(_, bytes)| bytes.len()).sum();
        if bytes > wire::MAX_DATA {
            return Err(Error::Errno(-errno::E2BIG));
        }
        let lazy = request.sink.is_some();
        if !lazy && request.data.is_empty() {
            return Err(Error::Errno(-errno::EINVAL));
        }
        let index = self.session_index(session)?;
        self.next_token = self.next_token.wrapping_add(1);
        let token = self.next_token;
        let offer = Offer {
            token,
            owner: request.owner,
            session,
            owner_slot,
            mimes,
            data: request.data,
            sink: request.sink,
            endpoint: None,
            lazy,
            tick,
        };
        let info = offer.info();
        sys::write_str(&format!(
            "clipboardd: offer #{} owner={} session={} app={} lazy={} mimes={}\n",
            token,
            info.owner,
            session,
            offer.owner_slot,
            lazy,
            info.mimes.len()
        ));
        let history = self.history;
        let clip = &mut self.sessions[index];
        clip.offers.push_back(offer);
        while clip.offers.len() > history {
            clip.offers.pop_front();
        }
        let session_text = format!("{session}");
        let _ = wire::publish_session_clipboard_changed(
            self,
            &session_text,
            &wire::to_wire_meta(&info),
        );
        Ok(token)
    }

    /// Resolve one `Request`: find the offer, then read it inline or through
    /// the owner's `Serialize`.
    pub(super) fn request(
        &mut self,
        token: u64,
        mime: &str,
        session: u64,
    ) -> Result<wire::BufferHandle, Error> {
        // Snapshot the hit first: a lazy read may call another task and mutate
        // the endpoint cache, so the session table cannot stay borrowed.
        let reading = {
            let clip = self.sessions.iter().find(|clip| clip.session == session);
            let offer = clip.and_then(|clip| {
                if token == 0 {
                    clip.offers
                        .iter()
                        .rev()
                        .find(|offer| offer.mimes.iter().any(|known| known == mime))
                } else {
                    clip.offers.iter().find(|offer| {
                        offer.token == token && offer.mimes.iter().any(|known| known == mime)
                    })
                }
            });
            match offer {
                Some(offer) if offer.lazy => Reading::Lazy {
                    mime: String::from(mime),
                    sink: offer.sink.clone().unwrap_or_default(),
                    cached: offer.endpoint,
                },
                Some(offer) => match offer.data.iter().find(|(known, _)| known == mime) {
                    Some((_, bytes)) => Reading::Eager {
                        mime: String::from(mime),
                        bytes: bytes.clone(),
                    },
                    None => return Err(Error::Errno(-errno::ENOENT)),
                },
                None => return Err(self.miss(token)),
            }
        };
        match reading {
            Reading::Eager { mime, bytes } => Ok(wire::BufferHandle {
                token,
                mime,
                lazy: false,
                bytes,
            }),
            Reading::Lazy { mime, sink, cached } => {
                let endpoint = match cached {
                    Some(endpoint) => endpoint,
                    None => {
                        let endpoint = registry::resolve(&sink)?;
                        self.cache_endpoint(session, token, endpoint);
                        endpoint
                    }
                };
                let deadline = sys::clock().saturating_add(SERIALIZE_DEADLINE);
                let reply = endpoint.call(
                    &wire::serialize_request(token, mime.as_str())?,
                    Some(deadline),
                )?;
                let bytes = wire::decode_serialized(&reply)?;
                Ok(wire::BufferHandle {
                    token,
                    mime,
                    lazy: true,
                    bytes,
                })
            }
        }
    }

    /// The token was not readable in `session`: a token that exists in another
    /// session is a policy denial, anything else "not found".
    fn miss(&self, token: u64) -> Error {
        let foreign = token != 0
            && self
                .sessions
                .iter()
                .any(|clip| clip.offers.iter().any(|offer| offer.token == token));
        if foreign {
            Error::Errno(-errno::EACCES)
        } else {
            Error::Errno(-errno::ENOENT)
        }
    }

    /// Cache the endpoint resolved for a lazy offer.
    fn cache_endpoint(&mut self, session: u64, token: u64, endpoint: Endpoint) {
        if let Some(clip) = self
            .sessions
            .iter_mut()
            .find(|clip| clip.session == session)
        {
            if let Some(offer) = clip.offers.iter_mut().find(|offer| offer.token == token) {
                offer.endpoint = Some(endpoint);
            }
        }
    }

    /// Log an allowed paste and publish the typed audit event.
    pub(super) fn log_paste(&mut self, cred: &sys::Cred, slot: u64, handle: &wire::BufferHandle) {
        self.pastes += 1;
        sys::write_str(&format!(
            "clipboardd: paste #{} uid={} session={} mime={} app={} bytes={} lazy={}\n",
            self.pastes,
            cred.uid,
            cred.session,
            handle.mime,
            slot,
            handle.bytes.len(),
            handle.lazy
        ));
        let event = wire::PasteEvent {
            seq: self.pastes,
            uid: cred.uid,
            session: cred.session,
            mime: handle.mime.clone(),
            app: slot,
            bytes: handle.bytes.len() as u64,
            lazy: handle.lazy,
        };
        // Best-effort: the next event reconnects when the broker is
        // unreachable, so a failed publish never corrupts the paste state.
        let _ = wire::publish_system_events_clipboard_paste(self, &event);
    }

    /// Log a refused cross-session paste and publish the typed denial.
    pub(super) fn log_denial(&mut self, cred: &sys::Cred, slot: u64, mime: &str, token: u64) {
        self.denies += 1;
        sys::write_str(&format!(
            "clipboardd: DENIED #{} uid={} session={} mime={} app={} token={} \
             (clipboard.read scope)\n",
            self.denies, cred.uid, cred.session, mime, slot, token
        ));
        let event = wire::ClipboardDenial {
            seq: self.denies,
            uid: cred.uid,
            session: cred.session,
            mime: String::from(mime),
            app: slot,
            token,
        };
        let _ = wire::publish_system_events_security_clipboard(self, &event);
    }
}

impl topics::Publish for Clipboard {
    type Error = Error;

    /// Publish raw bytes through the central broker, reusing one connection.
    fn publish_topic(&mut self, topic: &str, payload: &[u8], retained: bool) -> Result<u64, Error> {
        if self.central.is_none() {
            self.central = central::Bus::connect_retry(4).ok();
        }
        let result = match &mut self.central {
            Some(bus) => bus.publish(topic, payload, retained),
            None => Err(Error::Errno(-errno::ENOENT)),
        };
        if result.is_err() {
            self.central = None;
        }
        result
    }
}
