//! The `clipboardd` client: text copy/paste shared across LazyOS apps.
//!
//! `set_text` publishes an eager `text/plain` offer; `text` requests the newest
//! offer in the caller's session. Payloads are bounded to [`MAX_BYTES`] (16
//! MiB) on both paths. Up to [`INLINE_BYTES`] they ride inside the parcel;
//! above it a copy goes into shared pages (`OfferBuffer`: the service takes
//! one snapshot of them into pages of its own, not its heap) and a paste
//! reads the size from `Request` and has the service copy the bytes into a
//! buffer this task creates (`Fetch`). When `clipboardd` is absent (a console
//! image, or before it starts) the module falls back to a thread-local store,
//! so an app never hangs or panics without the service.
//!
//! The generated client frames every call: `Offer` travels on the
//! `os.lazy.clipboard.write.v1` ACL scope and `Request` on
//! `os.lazy.clipboard.read.v1`, the two methods that carry a buffer on the
//! interface's own id (the kernel's declared-object gate keys on the header's
//! interface), and each waits the `deadline` `idl/clipboard.midl` states: half a
//! second for `Offer` and `Request`, because a copy or paste key runs on the UI
//! thread (`Backend::clipboard_text` is a plain getter) and a service that took
//! the request and never answers must cost that, not the window (the callers
//! fall back to the in-process copy on any error); five seconds for
//! `OfferBuffer` and `Fetch`, where the service maps pages and copies megabytes.
//!
//! The wire calls sit behind the [`Transport`] seam so the fallback and the
//! size bound can be tested on a host with no LazyOS kernel.

use std::cell::{Cell, RefCell};

use libmessenger::Buffer;
use messenger_generated::os_lazy_clipboard_v1 as wire;

use super::messenger::{client, Observed};
use crate::sys::{self, errno};

/// The service's generated client, which never waits for the service to
/// register: a console image has none, and the callers fall back.
type Service = wire::Client<Observed>;

/// The MIME type used for plain text.
pub const TEXT_MIME: &str = "text/plain";
/// The largest payload either direction accepts: 16 MiB, the service's own
/// bound (`user::messenger::clipboard::MAX_BUFFER`).
pub const MAX_BYTES: usize = 16 * 1024 * 1024;
/// The largest payload that travels inside a parcel; a bigger one goes through
/// shared pages.
pub const INLINE_BYTES: usize = 8 * 1024;
/// A human-readable owner label for offers from an xui app.
const OWNER: &str = "xui-app";
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
        offer_payload(TEXT_MIME, text.as_bytes()).map(|_| ())
    }

    fn request(&self) -> Result<Vec<u8>, i64> {
        paste_payload(0, TEXT_MIME)
    }
}

/// Offers `bytes` as `mime` and returns the offer token: inline up to
/// [`INLINE_BYTES`], through shared pages above it, never above
/// [`MAX_BYTES`].
fn offer_payload(mime: &str, bytes: &[u8]) -> Result<u64, i64> {
    if bytes.len() > MAX_BYTES {
        return Err(-errno::E2BIG);
    }
    let service = client(wire::Client::try_connect)?;
    let token = if bytes.len() > INLINE_BYTES {
        offer_buffer(&service, mime, bytes)?
    } else {
        let payload = wire::Payload {
            mime: mime.to_owned(),
            bytes: bytes.to_vec(),
        };
        service.offer(OWNER, None, &[mime.to_owned()], &[payload])?
    };
    if token == 0 {
        return Err(-errno::EINVAL);
    }
    Ok(token)
}

/// `OfferBuffer`: the bytes go into new shared pages, the service snapshots
/// them during the call, and ours close when it returns.
fn offer_buffer(service: &Service, mime: &str, bytes: &[u8]) -> Result<u64, i64> {
    let (handle, va, _) = sys::buffer_create(bytes.len() as u64)?;
    // SAFETY: `buffer_create` mapped at least `bytes.len()` writable bytes at
    // `va`, which this task owns until `buffer_close` below.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), va as *mut u8, bytes.len()) };
    let sent = service.offer_buffer(OWNER, mime, Buffer::whole(handle, bytes.len() as u64));
    let _ = sys::buffer_close(handle);
    Ok(sent?)
}

/// What a `Request` reply asks of the reader.
#[derive(Debug, PartialEq, Eq)]
enum Plan {
    /// The whole payload came inline.
    Inline(Vec<u8>),
    /// The payload stays in the service's pages: fetch `size` bytes of offer
    /// `token`.
    Fetch { token: u64, size: usize },
}

/// Judge a `Request` reply: the payload's size is bounded by [`MAX_BYTES`]
/// before anything is allocated, and a reply whose inline bytes disagree with
/// its size is refused rather than trusted.
fn plan(reply: wire::RequestReply) -> Result<Plan, i64> {
    // A service of the previous protocol answers inline with no size.
    let size = reply.size.max(reply.bytes.len() as u64);
    if size > MAX_BYTES as u64 {
        return Err(-errno::E2BIG);
    }
    if reply.bytes.len() as u64 == size {
        return Ok(Plan::Inline(reply.bytes));
    }
    if !reply.bytes.is_empty() || reply.token == 0 {
        return Err(-errno::EINVAL); // a reply that contradicts itself
    }
    Ok(Plan::Fetch {
        token: reply.token,
        size: size as usize,
    })
}

/// Reads offer `token` (0: the newest) as `mime`, inline or through a buffer.
fn paste_payload(token: u64, mime: &str) -> Result<Vec<u8>, i64> {
    let service = client(wire::Client::try_connect)?;
    match plan(service.request(token, mime)?)? {
        Plan::Inline(bytes) => Ok(bytes),
        Plan::Fetch { token, size } => fetch(&service, token, mime, size),
    }
}

/// `Fetch`: the service copies `size` bytes into a buffer this task creates.
fn fetch(service: &Service, token: u64, mime: &str, size: usize) -> Result<Vec<u8>, i64> {
    let (handle, va, _) = sys::buffer_create(size as u64)?;
    let sent = service.fetch(token, mime, Buffer::whole(handle, size as u64));
    let result = sent.map_err(i64::from).and_then(|written| {
        if written > size as u64 {
            return Err(-errno::EINVAL);
        }
        // SAFETY: `buffer_create` mapped `size` readable bytes at `va`, owned
        // by this task until `buffer_close` below, and `written <= size`.
        Ok(unsafe { std::slice::from_raw_parts(va as *const u8, written as usize) }.to_vec())
    });
    let _ = sys::buffer_close(handle);
    result
}

/// Offers `bytes` as `mime` (a drag's payload) and returns the offer token
/// `DragStart` hands the compositor. Unlike [`set_text`] there is no
/// in-process fallback: a drag needs the service to reach another app.
pub fn offer(mime: &str, bytes: &[u8]) -> Result<u64, i64> {
    offer_payload(mime, bytes)
}

/// Pastes offer `token` as `mime` (a drop's payload), bounded like every
/// paste. The service refuses another session's token (`-EACCES`).
pub fn paste(token: u64, mime: &str) -> Result<Vec<u8>, i64> {
    paste_payload(token, mime)
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

#[cfg(test)]
mod tests {
    use super::*;
    use libmessenger::client::Deadline;

    /// A transport whose every call fails, as if `clipboardd` were absent.
    struct Absent;

    impl Transport for Absent {
        fn offer(&self, _text: &str) -> Result<(), i64> {
            Err(-errno::ENOENT)
        }
        fn request(&self) -> Result<Vec<u8>, i64> {
            Err(-errno::ENOENT)
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
            self.0.borrow().clone().ok_or(-errno::ENOENT)
        }
    }

    fn reply(token: u64, bytes: &[u8], size: u64) -> wire::RequestReply {
        wire::RequestReply {
            token,
            mime: TEXT_MIME.to_owned(),
            lazy: false,
            bytes: bytes.to_vec(),
            size,
        }
    }

    #[test]
    fn a_small_reply_is_inline_whatever_its_size_field_says() {
        assert_eq!(plan(reply(3, b"hi", 2)), Ok(Plan::Inline(b"hi".to_vec())));
        // The previous protocol had no size field.
        assert_eq!(plan(reply(3, b"hi", 0)), Ok(Plan::Inline(b"hi".to_vec())));
        assert_eq!(plan(reply(3, b"", 0)), Ok(Plan::Inline(Vec::new())));
    }

    #[test]
    fn a_large_reply_is_fetched_by_its_resolved_token() {
        let size = (INLINE_BYTES + 1) as u64;
        assert_eq!(
            plan(reply(9, b"", size)),
            Ok(Plan::Fetch {
                token: 9,
                size: INLINE_BYTES + 1
            })
        );
        let max = MAX_BYTES as u64;
        assert_eq!(
            plan(reply(9, b"", max)),
            Ok(Plan::Fetch {
                token: 9,
                size: MAX_BYTES
            })
        );
    }

    #[test]
    fn untrusted_sizes_are_bounded_before_anything_is_allocated() {
        assert_eq!(
            plan(reply(9, b"", MAX_BYTES as u64 + 1)),
            Err(-errno::E2BIG)
        );
        assert_eq!(plan(reply(9, b"", u64::MAX)), Err(-errno::E2BIG));
        assert_eq!(plan(reply(9, b"", u64::MAX / 2)), Err(-errno::E2BIG));
    }

    #[test]
    fn a_reply_that_contradicts_itself_is_refused() {
        assert_eq!(
            plan(reply(9, b"abc", 10)),
            Err(-errno::EINVAL),
            "inline bytes shorter than the size"
        );
        assert_eq!(
            plan(reply(0, b"", 100)),
            Err(-errno::EINVAL),
            "a fetch needs the resolved token"
        );
    }

    #[test]
    fn the_bounds_agree_with_the_service() {
        assert_eq!(MAX_BYTES, user_max_buffer());
        assert_eq!(INLINE_BYTES, 8 * 1024);
    }

    /// The service's bound as the IDL doc and `clipboardd` state it.
    const fn user_max_buffer() -> usize {
        16 * 1024 * 1024
    }

    #[test]
    fn the_deadlines_are_the_bounds_the_client_always_used() {
        let ms = |deadline_ms: u32| Deadline::millis(u64::from(deadline_ms));
        assert_eq!(ms(wire::METHOD_OFFER_DEADLINE_MS), Deadline::ticks(50));
        assert_eq!(ms(wire::METHOD_REQUEST_DEADLINE_MS), Deadline::ticks(50));
        assert_eq!(
            ms(wire::METHOD_OFFERBUFFER_DEADLINE_MS),
            Deadline::ticks(500)
        );
        assert_eq!(ms(wire::METHOD_FETCH_DEADLINE_MS), Deadline::ticks(500));
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
