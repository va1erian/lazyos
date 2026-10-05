//! The `clipboardd` client: text copy/paste shared across LazyOS apps.
//!
//! `set_text` publishes an eager `text/plain` offer; `text` requests the newest
//! offer in the caller's session. Payloads are bounded to [`MAX_BYTES`] on both
//! paths, and when `clipboardd` is absent (a console image, or before it
//! starts) the module falls back to a thread-local store, so an app never hangs
//! or panics without the service.
//!
//! `Offer` travels on the `os.lazy.clipboard.write.v1` ACL scope and `Request`
//! on `os.lazy.clipboard.read.v1` (the MIDL documents these as capability
//! names, not wire fields), so their `fnv1a64` hashes live here.
//!
//! The wire calls sit behind the [`Transport`] seam so the fallback and the
//! size bound can be tested on a host with no LazyOS kernel.

use std::cell::{Cell, RefCell};

use libmessenger::Parcel;
use messenger_generated::os_lazy_clipboard_v1 as wire;

use super::messenger::Service;

/// The service's registered name.
const NAME: &str = "os.lazy.clipboard";
/// The structured-error field id the service uses (from `idl/clipboard.midl`).
const ERROR_FIELD: u16 = 13;
/// `fnv1a64("os.lazy.clipboard.write.v1")`.
const WRITE_INTERFACE: u64 = fnv1a64("os.lazy.clipboard.write.v1");
/// `fnv1a64("os.lazy.clipboard.read.v1")`.
const READ_INTERFACE: u64 = fnv1a64("os.lazy.clipboard.read.v1");
/// The MIME type used for plain text.
pub const TEXT_MIME: &str = "text/plain";
/// The largest payload either direction accepts (the service's inline cap).
pub const MAX_BYTES: usize = 8 * 1024;
/// A human-readable owner label for offers from an xui app.
const OWNER: &str = "xui-app";

/// FNV-1a 64, matching `tools/midlc`'s interface-id hash.
const fn fnv1a64(text: &str) -> u64 {
    let bytes = text.as_bytes();
    let mut hash = 0xCBF2_9CE4_8422_2325u64;
    let mut index = 0;
    while index < bytes.len() {
        hash = (hash ^ bytes[index] as u64).wrapping_mul(0x0000_0100_0000_01B3);
        index += 1;
    }
    hash
}

thread_local! {
    /// The fallback store, used when `clipboardd` is absent.
    static IN_PROCESS: RefCell<String> = const { RefCell::new(String::new()) };
    /// Set while [`IN_PROCESS`] holds the newest copy (it was too large to
    /// offer, or the offer failed), so a paste must not prefer an older offer
    /// still sitting in the service.
    static LOCAL_NEWEST: Cell<bool> = const { Cell::new(false) };
}

/// The two clipboard operations, so tests can substitute a failing service.
trait Transport {
    /// Publish `text` as an offer. The error is a negative errno.
    fn offer(&self, text: &str) -> Result<(), i64>;
    /// Read the newest text offer. The error is a negative errno.
    fn request(&self) -> Result<Vec<u8>, i64>;
}

/// The real transport: `clipboardd` over Messenger.
struct MessengerTransport;

impl Transport for MessengerTransport {
    fn offer(&self, text: &str) -> Result<(), i64> {
        let service = Service::try_connect(NAME).ok_or(-2)?;
        let body = wire::encode_offer_args(&wire::OfferArgs {
            owner: OWNER.to_owned(),
            sink: None,
            mimes: vec![TEXT_MIME.to_owned()],
            data: vec![wire::Payload {
                mime: TEXT_MIME.to_owned(),
                bytes: text.as_bytes().to_vec(),
            }],
        })
        .map_err(|_| -22)?;
        service
            .call(WRITE_INTERFACE, wire::METHOD_OFFER, ERROR_FIELD, body)
            .map(|_| ())
    }

    fn request(&self) -> Result<Vec<u8>, i64> {
        let service = Service::try_connect(NAME).ok_or(-2)?;
        let body = wire::encode_request_args(&wire::RequestArgs {
            token: 0,
            mime: TEXT_MIME.to_owned(),
        })
        .map_err(|_| -22)?;
        let reply = service.call(READ_INTERFACE, wire::METHOD_REQUEST, ERROR_FIELD, body)?;
        decode_bytes(&reply)
    }
}

/// Offers `bytes` as `mime` (a drag's payload) and returns the offer token
/// `DragStart` hands the compositor. Unlike [`set_text`] there is no
/// in-process fallback: a drag needs the service to reach another app.
pub fn offer(mime: &str, bytes: &[u8]) -> Result<u64, i64> {
    if bytes.len() > MAX_BYTES {
        return Err(-7); // E2BIG
    }
    let service = Service::try_connect(NAME).ok_or(-2)?;
    let body = wire::encode_offer_args(&wire::OfferArgs {
        owner: OWNER.to_owned(),
        sink: None,
        mimes: vec![mime.to_owned()],
        data: vec![wire::Payload {
            mime: mime.to_owned(),
            bytes: bytes.to_vec(),
        }],
    })
    .map_err(|_| -22)?;
    let reply = service.call(WRITE_INTERFACE, wire::METHOD_OFFER, ERROR_FIELD, body)?;
    let token = wire::decode_offer_reply(&reply.body)
        .map_err(|_| -22)?
        .token;
    if token == 0 {
        return Err(-22);
    }
    Ok(token)
}

/// Pastes offer `token` as `mime` (a drop's payload), bounded like every
/// paste. The service refuses another session's token (`-EACCES`).
pub fn paste(token: u64, mime: &str) -> Result<Vec<u8>, i64> {
    let service = Service::try_connect(NAME).ok_or(-2)?;
    let body = wire::encode_request_args(&wire::RequestArgs {
        token,
        mime: mime.to_owned(),
    })
    .map_err(|_| -22)?;
    let reply = service.call(READ_INTERFACE, wire::METHOD_REQUEST, ERROR_FIELD, body)?;
    decode_bytes(&reply)
}

/// Sets the clipboard text, falling back in-process when the service is absent.
pub fn set_text(text: &str) {
    set_text_with(&MessengerTransport, text);
}

/// Reads the clipboard text, preferring the service and falling back in-process.
pub fn text() -> Option<String> {
    text_with(&MessengerTransport)
}

/// [`set_text`] against a chosen transport.
fn set_text_with(transport: &dyn Transport, text: &str) {
    // A payload larger than the service's cap cannot be offered; keep it
    // in-process so a copy/paste within one app still works.
    if text.len() <= MAX_BYTES && transport.offer(text).is_ok() {
        // The service now holds the newest copy; drop the local one so a
        // later failed request cannot resurrect stale text.
        IN_PROCESS.with(|slot| slot.borrow_mut().clear());
        LOCAL_NEWEST.with(|flag| flag.set(false));
        return;
    }
    IN_PROCESS.with(|slot| *slot.borrow_mut() = text.to_string());
    LOCAL_NEWEST.with(|flag| flag.set(true));
}

/// [`text`] against a chosen transport.
fn text_with(transport: &dyn Transport) -> Option<String> {
    if LOCAL_NEWEST.with(Cell::get) {
        return local_text();
    }
    match transport.request() {
        Ok(bytes) if bytes.len() <= MAX_BYTES => String::from_utf8(bytes).ok(),
        _ => local_text(),
    }
}

/// The in-process copy, if there is one.
fn local_text() -> Option<String> {
    IN_PROCESS.with(|slot| {
        let value = slot.borrow();
        (!value.is_empty()).then(|| value.clone())
    })
}

/// Decodes a `Request` reply body into bytes, enforcing [`MAX_BYTES`].
fn decode_bytes(reply: &Parcel) -> Result<Vec<u8>, i64> {
    let reply = wire::decode_request_reply(&reply.body).map_err(|_| -22)?;
    if reply.bytes.len() > MAX_BYTES {
        return Err(-7);
    }
    Ok(reply.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A transport whose every call fails, as if `clipboardd` were absent.
    struct Absent;

    impl Transport for Absent {
        fn offer(&self, _text: &str) -> Result<(), i64> {
            Err(-2)
        }
        fn request(&self) -> Result<Vec<u8>, i64> {
            Err(-2)
        }
    }

    /// A transport that stores one value in memory.
    #[derive(Default)]
    struct Fake(RefCell<Option<Vec<u8>>>);

    impl Transport for Fake {
        fn offer(&self, text: &str) -> Result<(), i64> {
            *self.0.borrow_mut() = Some(text.as_bytes().to_vec());
            Ok(())
        }
        fn request(&self) -> Result<Vec<u8>, i64> {
            self.0.borrow().clone().ok_or(-2)
        }
    }

    #[test]
    fn the_scope_hashes_match_the_idl_names() {
        assert_eq!(fnv1a64("os.lazy.clipboard.write.v1"), WRITE_INTERFACE);
        assert_eq!(fnv1a64("os.lazy.clipboard.read.v1"), READ_INTERFACE);
    }

    #[test]
    fn an_absent_service_falls_back_to_the_in_process_store() {
        IN_PROCESS.with(|slot| slot.borrow_mut().clear());
        set_text_with(&Absent, "offline");
        assert_eq!(text_with(&Absent).as_deref(), Some("offline"));
    }

    #[test]
    fn an_over_bounded_payload_is_never_offered() {
        let fake = Fake::default();
        // `set_text_with` stores it and must not call `offer`.
        set_text_with(&fake, &"x".repeat(MAX_BYTES + 1));
        assert!(fake.0.borrow().is_none());
        IN_PROCESS.with(|slot| assert_eq!(slot.borrow().len(), MAX_BYTES + 1));
    }

    #[test]
    fn an_oversize_copy_wins_over_an_older_offer() {
        let fake = Fake::default();
        set_text_with(&fake, "older");
        let big = "y".repeat(MAX_BYTES + 1);
        set_text_with(&fake, &big);
        assert_eq!(text_with(&fake).as_deref(), Some(big.as_str()));
    }

    #[test]
    fn a_stale_fallback_is_not_returned_after_a_later_offer() {
        set_text_with(&Absent, "stale");
        let fake = Fake::default();
        set_text_with(&fake, "fresh");
        assert_eq!(text_with(&fake).as_deref(), Some("fresh"));
        // The service goes away: nothing local is left to resurrect.
        assert_eq!(text_with(&Absent), None);
    }

    #[test]
    fn a_working_service_round_trips() {
        let fake = Fake::default();
        set_text_with(&fake, "shared");
        assert_eq!(text_with(&fake).as_deref(), Some("shared"));
    }
}
