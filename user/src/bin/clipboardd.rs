//! `clipboardd` (`CLIPD.ELF`): the per-session clipboard service (issue #115).
//!
//! This is the S4 clipboard from `docs/platform-plan.md` section 4.5 and the
//! worked example in `docs/messenger.md` section 19:
//!
//! * a client offers typed payloads (MIME strings) for **its session** and
//!   receives a token (`Offer(owner, mime_types) -> token`);
//! * a paster asks for a payload (`Request(token, mime)`); the service answers
//!   with a [`wire::BufferHandle`]. An **eager** offer keeps a bounded copy;
//!   a **lazy** offer names a registry endpoint (`sink`) and the service calls
//!   the owner's `Serialize` only when a paste happens, so the owner
//!   materializes the data on demand;
//! * every offer is announced, retained, on
//!   `session/<id>/clipboard/changed`, so paste UIs refresh without polling;
//! * the history policy is one offer per session by default; the supervisor
//!   can pass `history=N` in the manifest argument string to keep up to
//!   [`MAX_HISTORY`].
//!
//! # Policy
//!
//! `Offer` and `Request` parcels carry the `clipboard.write` / `clipboard.read`
//! pseudo-interface ids in their header, so the kernel's `ipc::authorize` hook
//! (and its audit ring) gates them like any other Messenger call. The service
//! adds the session scope: the session id of every request comes from the
//! kernel-stamped credentials (`sys::cred_get`, which needs `CAP_SETUID` — the
//! supervisor starts this service as root today), never from the parcel. A
//! token offered by one session is refused for another; the refusal is counted,
//! printed, and published on `system/events/security/clipboard` so `logd`
//! retains it next to the kernel's own denials.
//!
//! # Buffer handle
//!
//! The kernel's `SHARE_ONLY` shared buffers (`kernel/src/ipc/shared.rs`) are
//! the future transport for pastes; there is no userspace mapping syscall yet
//! (`keyd` documents the same gap), so [`wire::BufferHandle`] carries the bytes
//! in the reply parcel and the service bounds inline eager payloads at
//! [`wire::MAX_DATA`]. The protocol does not change when the mapping op lands.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;
use user::central;
use user::messenger::{self, clipboard as wire, errno, registry, Endpoint, Error, Message, Parcel};
use user::sys;

/// Sessions the service keeps concurrently.
const MAX_SESSIONS: usize = 16;
/// Hard cap on `history=N`.
const MAX_HISTORY: usize = 8;
/// Default history: one current offer per session.
const DEFAULT_HISTORY: usize = 1;
/// PIT ticks the service waits for an owner's `Serialize` answer.
const SERIALIZE_DEADLINE: u64 = 100;
/// How long the serve loop parks between housekeeping checks (PIT ticks).
const POLL_TICKS: u64 = 5;
/// The evidence programs `demo=1` spawns at startup and reaps.
const DEMO_PROGRAMS: [&str; 2] = ["CLIPCP.ELF", "CLIPPS.ELF"];

/// One live offer in a session.
struct Offer {
    token: u64,
    owner: String,
    session: u64,
    owner_slot: u64,
    mimes: Vec<String>,
    data: Vec<(String, Vec<u8>)>,
    sink: Option<String>,
    /// Cached owner endpoint once a lazy paste resolved the sink.
    endpoint: Option<Endpoint>,
    lazy: bool,
    tick: u64,
}

impl Offer {
    /// Metadata only: the shape `Current` and the changed topic carry.
    fn info(&self) -> wire::OfferInfo {
        wire::OfferInfo {
            token: self.token,
            owner: self.owner.clone(),
            session: self.session,
            mimes: self.mimes.clone(),
            lazy: self.lazy,
            tick: self.tick,
        }
    }
}

/// One session's offer history; newest last.
struct SessionClip {
    session: u64,
    offers: VecDeque<Offer>,
}

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

/// The clipboard state: the per-session tables, the central-broker connection
/// the changed topic and audit events publish through, and the counters the
/// paste log reports.
struct Clipboard {
    sessions: Vec<SessionClip>,
    history: usize,
    next_token: u64,
    /// Cached `messengerd` connection; `None` until connected (or after a
    /// publish failure, when the next event reconnects).
    central: Option<central::Bus>,
    pastes: u64,
    denies: u64,
}

impl Clipboard {
    /// An empty clipboard with `history` offers kept per session.
    fn new(history: usize) -> Clipboard {
        Clipboard {
            sessions: Vec::new(),
            history,
            next_token: 0,
            central: None,
            pastes: 0,
            denies: 0,
        }
    }

    /// Record an offer for `session` and announce it on the changed topic.
    fn offer(
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
        let topic = wire::changes_topic(session);
        let payload = wire::changed_payload(&info)?;
        self.publish_central(&topic, &payload, true);
        Ok(token)
    }

    /// Find the session row, creating it (up to [`MAX_SESSIONS`]).
    fn session_index(&mut self, session: u64) -> Result<usize, Error> {
        if let Some(index) = self
            .sessions
            .iter()
            .position(|clip| clip.session == session)
        {
            return Ok(index);
        }
        if self.sessions.len() >= MAX_SESSIONS {
            return Err(Error::Errno(-errno::ENOMEM));
        }
        self.sessions.push(SessionClip {
            session,
            offers: VecDeque::new(),
        });
        Ok(self.sessions.len() - 1)
    }

    /// Resolve one `Request`: find the offer, then read it inline or through
    /// the owner's `Serialize`.
    fn request(
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
                let bytes = wire::decode_bytes(&reply)?;
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

    /// The session's newest offer, metadata only.
    fn current(&self, session: u64) -> Option<wire::OfferInfo> {
        let clip = self.sessions.iter().find(|clip| clip.session == session)?;
        clip.offers.back().map(Offer::info)
    }

    /// Log an allowed paste and publish it on the event feed.
    fn log_paste(&mut self, cred: &sys::Cred, slot: u64, handle: &wire::BufferHandle) {
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
        let detail = format!(
            "paste #{} uid={} session={} mime={} app={} bytes={} lazy={}",
            self.pastes,
            cred.uid,
            cred.session,
            handle.mime,
            slot,
            handle.bytes.len(),
            handle.lazy
        );
        self.publish_event("system/events/clipboard/paste", &detail);
    }

    /// Log a refused cross-session paste and publish the denial.
    fn log_denial(&mut self, cred: &sys::Cred, slot: u64, mime: &str, token: u64) {
        self.denies += 1;
        sys::write_str(&format!(
            "clipboardd: DENIED #{} uid={} session={} mime={} app={} token={} \
             (clipboard.read scope)\n",
            self.denies, cred.uid, cred.session, mime, slot, token
        ));
        let detail = format!(
            "deny #{} uid={} session={} mime={} app={} token={}",
            self.denies, cred.uid, cred.session, mime, slot, token
        );
        self.publish_event("system/events/security/clipboard", &detail);
    }

    /// Best-effort audit record: publish through `messengerd`'s central
    /// broker, reconnecting on the next event when the broker is unreachable.
    fn publish_event(&mut self, topic: &str, detail: &str) {
        self.publish_central(topic, detail.as_bytes(), false);
    }

    /// Publish raw bytes through the central broker, reusing one connection.
    fn publish_central(&mut self, topic: &str, payload: &[u8], retained: bool) {
        if self.central.is_none() {
            self.central = central::Bus::connect_retry(4).ok();
        }
        let ok = match &mut self.central {
            Some(bus) => bus.publish(topic, payload, retained).is_ok(),
            None => false,
        };
        if !ok {
            self.central = None;
        }
    }
}

/// The service's manifest-argument history depth (`history=N`, clamped).
fn history_from_args() -> usize {
    let mut buffer = [0u8; 128];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
    for part in text.split_whitespace() {
        if let Some(value) = part.strip_prefix("history=") {
            if let Ok(depth) = value.parse::<usize>() {
                return depth.clamp(1, MAX_HISTORY);
            }
        }
    }
    DEFAULT_HISTORY
}

/// Whether the manifest asked for the demo pair (`demo=1`).
fn demo_from_args() -> bool {
    let mut buffer = [0u8; 128];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
    text.split_whitespace().any(|part| part == "demo=1")
}

/// Spawn the two demo clients as children of this service; returns how many
/// started. They are evidence programs, not supervised services, so the
/// service reaps them itself.
fn spawn_demo() -> u64 {
    let mut started = 0u64;
    for program in DEMO_PROGRAMS {
        let mut command = program.as_bytes().to_vec();
        command.push(0);
        match sys::spawn(&command) {
            Some(pid) => {
                started += 1;
                sys::write_str(&format!("clipboardd: started demo {program} (pid {pid})\n"));
            }
            None => sys::write_str(&format!("clipboardd: demo {program} spawn failed\n")),
        }
    }
    started
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("clipboardd: per-session clipboard service (issue #115)\n");
    if let Err(error) = run() {
        sys::write_str("clipboardd: fatal: ");
        sys::write_str(error.message());
        sys::write_str("\n");
        sys::exit(1);
    }
    sys::exit(0)
}

/// Register the service and serve offers, pastes and the changed topic.
fn run() -> messenger::Result<()> {
    let history = history_from_args();
    let (published, server) = messenger::create_pair()?;
    registry::register(wire::NAME, &published, &[wire::INTERFACE], 0)?;
    let mut clipboard = Clipboard::new(history);
    sys::write_str(&format!("CLIPBOARD:HISTORY:{history}\n"));
    sys::write_str("CLIPBOARD:READY\n");
    // The demo pair (if requested) starts right here, so its two ELF loads
    // complete before the supervisor's crash test files its short-deadline
    // health report; the evidence then runs while the rest of boot is settled.
    let mut demo_pending = demo_from_args();
    let mut demo_children = 0u64;
    // One receive buffer for the whole life of the service: the user bump
    // allocator never reclaims per-call buffers, so long-lived loops must not
    // allocate one per request.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    loop {
        let now = sys::clock();
        if demo_pending {
            demo_children = spawn_demo();
            demo_pending = false;
        }
        // Wake each poll while a demo child is alive, to reap its exit; with
        // nothing to reap, park forever.
        let deadline = if demo_children > 0 {
            now.saturating_add(POLL_TICKS)
        } else {
            0
        };
        match server.recv_with(&mut buffer, (deadline != 0).then_some(deadline)) {
            Ok(message) => {
                let interface = message.interface_id();
                let method = message.method();
                let reply = match dispatch(&mut clipboard, &message) {
                    Ok(reply) => reply,
                    Err(error) => wire::error_reply(interface, method, error),
                };
                if let Some(txn) = message.txn {
                    // A caller whose deadline passed is a normal scheduling
                    // race: the kernel expired the transaction and the reply
                    // is `-ENOENT`. Keep serving the other session's offers.
                    if let Err(error) = server.reply(txn, &reply) {
                        if error.errno() != Some(-errno::ENOENT) {
                            return Err(error);
                        }
                    }
                }
            }
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => {}
            Err(error) => return Err(error),
        }
        // Non-blocking reap: an expired deadline returns after the next timer
        // sweep, so a demo child that exited is collected promptly.
        while demo_children > 0 && sys::wait(sys::clock()).is_some() {
            demo_children -= 1;
        }
    }
}

/// The kernel-stamped actor for a message: the service holds `CAP_SETUID`, so
/// it may read another task's credential block.
fn actor(message: &Message) -> messenger::Result<sys::Cred> {
    let mut cred = sys::Cred::default();
    sys::cred_get(Some(message.sender), &mut cred).map_err(Error::Errno)?;
    Ok(cred)
}

/// Route one inbound message.
fn dispatch(clipboard: &mut Clipboard, message: &Message) -> messenger::Result<Parcel> {
    match (message.interface_id(), message.method()) {
        (wire::WRITE_INTERFACE, wire::method::OFFER) => {
            let request = wire::decode_offer(&message.parcel)?;
            let cred = actor(message)?;
            let token = clipboard.offer(request, cred.session, message.sender, sys::clock())?;
            wire::token_reply(token)
        }
        (wire::READ_INTERFACE, wire::method::REQUEST) => {
            let (token, mime) = wire::decode_request(&message.parcel)?;
            let cred = actor(message)?;
            let handle = clipboard.request(token, &mime, cred.session);
            match &handle {
                Ok(handle) => clipboard.log_paste(&cred, message.sender, handle),
                Err(Error::Errno(code)) if *code == -errno::EACCES => {
                    clipboard.log_denial(&cred, message.sender, &mime, token);
                }
                Err(_) => {}
            }
            wire::request_reply(&handle?)
        }
        (wire::INTERFACE, wire::method::CURRENT) => {
            let cred = actor(message)?;
            wire::current_reply(clipboard.current(cred.session).as_ref())
        }
        (wire::INTERFACE, wire::method::PING) => {
            Ok(wire::ok_reply(wire::INTERFACE, wire::method::PING))
        }
        _ => Err(Error::Errno(-errno::EINVAL)),
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
