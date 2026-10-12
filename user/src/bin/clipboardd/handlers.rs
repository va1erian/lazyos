//! Clipboard request handlers: offers, pastes, lazy serialize and publishing.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use messenger_generated::topics;
use user::central;
use user::messenger::{clipboard as wire, errno, registry, Endpoint, Error};
use user::sys;

use super::state::{Big, Clipboard, Offer};
use super::SERIALIZE_DEADLINE;

/// Where a `Request` found its payload.
enum Reading {
    /// An inline payload the service already holds.
    Eager { mime: String, bytes: Vec<u8> },
    /// A payload in shared pages: only its size travels in the reply.
    Big { mime: String, size: usize },
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
            big: None,
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

    /// `OfferBuffer`: snapshot the range `request.data` names as one payload
    /// of `mime`. The service copies it once into pages of its own and closes
    /// the received handle at once, so what is pasted later is what was
    /// copied, however the owner uses its pages afterwards.
    pub(super) fn offer_buffer(
        &mut self,
        request: wire::wire::OfferBufferArgs,
        session: u64,
        owner_slot: u64,
        tick: u64,
    ) -> Result<u64, Error> {
        let handle = request.data.handle;
        let result = self.keep_buffer(request, session, owner_slot, tick);
        // Decoded, so the received handle is ours to close whatever happened.
        let _ = sys::buffer_close(handle);
        result
    }

    fn keep_buffer(
        &mut self,
        request: wire::wire::OfferBufferArgs,
        session: u64,
        owner_slot: u64,
        tick: u64,
    ) -> Result<u64, Error> {
        let (mime, data) = (request.mime, request.data);
        if mime.is_empty() {
            return Err(Error::Errno(-errno::EINVAL));
        }
        if mime.len() > wire::MAX_MIME || request.owner.len() > wire::MAX_TEXT {
            return Err(Error::Errno(-errno::E2BIG));
        }
        let len = usize::try_from(data.len).map_err(|_| Error::Errno(-errno::E2BIG))?;
        if len == 0 {
            return Err(Error::Errno(-errno::EINVAL));
        }
        if len > wire::MAX_BUFFER {
            return Err(Error::Errno(-errno::E2BIG));
        }
        // The kernel knows nothing about the byte range: the mapped size is
        // the truth about what the pages hold.
        let (va, size) = sys::buffer_map(data.handle).map_err(Error::Errno)?;
        if va == 0 || !data.fits(size) {
            return Err(Error::Errno(-errno::EINVAL));
        }
        let index = self.session_index(session)?;
        // One copy into pages the owner never sees: the snapshot.
        let (own, own_va, _) = sys::buffer_create(len as u64).map_err(Error::Errno)?;
        // SAFETY: the source range lies inside the mapping `buffer_map` just
        // reported (`data.fits(size)`); the destination is the fresh mapping of
        // at least `len` bytes `buffer_create` returned. They are different
        // buffers, so the ranges cannot overlap.
        unsafe {
            core::ptr::copy_nonoverlapping((va + data.offset) as *const u8, own_va as *mut u8, len);
        }
        self.next_token = self.next_token.wrapping_add(1);
        let token = self.next_token;
        let offer = Offer {
            token,
            owner: request.owner,
            session,
            owner_slot,
            mimes: alloc::vec![mime.clone()],
            data: Vec::new(),
            big: Some(Big {
                mime,
                handle: own,
                va: own_va,
                len,
            }),
            sink: None,
            endpoint: None,
            lazy: false,
            tick,
        };
        let info = offer.info();
        sys::write_str(&format!(
            "clipboardd: offer #{} owner={} session={} app={} buffer={} mimes=1\n",
            token, info.owner, session, owner_slot, len
        ));
        let history = self.history;
        let clip = &mut self.sessions[index];
        clip.offers.push_back(offer);
        // Evict by count, then by the bytes kept in shared pages.
        while clip.offers.len() > history
            || (clip.offers.len() > 1
                && clip.offers.iter().map(Offer::big_len).sum::<usize>() > wire::MAX_RETAINED)
        {
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

    /// `Fetch`: copy offer `token`'s buffer-backed `mime` into the caller's
    /// buffer. The service owns the received handle and closes it here.
    pub(super) fn fetch(
        &mut self,
        request: wire::wire::FetchArgs,
        session: u64,
    ) -> Result<u64, Error> {
        let handle = request.into.handle;
        let result = self.copy_out(&request, session);
        let _ = sys::buffer_close(handle);
        result
    }

    fn copy_out(&mut self, request: &wire::wire::FetchArgs, session: u64) -> Result<u64, Error> {
        if request.token == 0 {
            return Err(Error::Errno(-errno::EINVAL));
        }
        let found = self
            .sessions
            .iter()
            .find(|clip| clip.session == session)
            .and_then(|clip| {
                clip.offers
                    .iter()
                    .find(|offer| offer.token == request.token)
            });
        let Some(offer) = found else {
            return Err(self.miss(request.token, session));
        };
        let Some(big) = offer.big.as_ref().filter(|big| big.mime == request.mime) else {
            return Err(Error::Errno(-errno::ENOENT));
        };
        let into = request.into;
        if (into.len as usize) < big.len {
            return Err(Error::Errno(-errno::E2BIG));
        }
        let (va, size) = sys::buffer_map(into.handle).map_err(Error::Errno)?;
        if va == 0 || !into.fits(size) {
            return Err(Error::Errno(-errno::EINVAL));
        }
        // SAFETY: the source range was checked against its mapping when the
        // offer was kept and the mapping lives as long as the offer; the
        // destination range was just checked against its mapping to hold at
        // least `big.len` bytes (`into.len >= big.len`, `into.fits(size)`).
        // `copy` tolerates the two ranges overlapping (the same buffer).
        unsafe {
            core::ptr::copy(big.va as *const u8, (va + into.offset) as *mut u8, big.len);
        }
        Ok(big.len as u64)
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
        let (token, reading) = {
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
                Some(offer) if offer.lazy => (
                    offer.token,
                    Reading::Lazy {
                        mime: String::from(mime),
                        sink: offer.sink.clone().unwrap_or_default(),
                        cached: offer.endpoint,
                    },
                ),
                Some(offer) if offer.big.is_some() => match &offer.big {
                    Some(big) if big.mime == mime => (
                        offer.token,
                        Reading::Big {
                            mime: String::from(mime),
                            size: big.len,
                        },
                    ),
                    _ => return Err(Error::Errno(-errno::ENOENT)),
                },
                Some(offer) => match offer.data.iter().find(|(known, _)| known == mime) {
                    Some((_, bytes)) => (
                        offer.token,
                        Reading::Eager {
                            mime: String::from(mime),
                            bytes: bytes.clone(),
                        },
                    ),
                    None => return Err(Error::Errno(-errno::ENOENT)),
                },
                None => return Err(self.miss(token, session)),
            }
        };
        match reading {
            Reading::Eager { mime, bytes } => Ok(wire::BufferHandle {
                token,
                mime,
                lazy: false,
                size: bytes.len() as u64,
                bytes,
            }),
            // The payload stays in its pages: the requester asks for it with
            // `Fetch`, naming the token resolved here.
            Reading::Big { mime, size } => Ok(wire::BufferHandle {
                token,
                mime,
                lazy: false,
                bytes: Vec::new(),
                size: size as u64,
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
                if bytes.len() > wire::MAX_DATA {
                    // A lazy owner answers inline: it cannot exceed a parcel.
                    return Err(Error::Errno(-errno::E2BIG));
                }
                Ok(wire::BufferHandle {
                    token,
                    mime,
                    lazy: true,
                    size: bytes.len() as u64,
                    bytes,
                })
            }
        }
    }

    /// The token was not readable in `session`: a token that exists in
    /// *another* session is a policy denial; anything else, a token of the
    /// caller's own session read as a MIME it does not carry included, is
    /// "not found" (and not an audited denial).
    fn miss(&self, token: u64, session: u64) -> Error {
        let foreign = token != 0
            && self.sessions.iter().any(|clip| {
                clip.session != session && clip.offers.iter().any(|offer| offer.token == token)
            });
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
            self.pastes, cred.uid, cred.session, handle.mime, slot, handle.size, handle.lazy
        ));
        let event = wire::PasteEvent {
            seq: self.pastes,
            uid: cred.uid,
            session: cred.session,
            mime: handle.mime.clone(),
            app: slot,
            bytes: handle.size,
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
