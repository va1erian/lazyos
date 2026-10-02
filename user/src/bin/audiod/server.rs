//! Requests in, replies out: the Messenger side of the mixer.
//!
//! Decoding and the stream rules live in `audiomix::service`, shared with the
//! host tests; this module adds what only a real server has: transferred
//! handles, deferred `Drain` replies, the card connection and the timers.

use alloc::format;
use alloc::vec::Vec;

use audiomix::service::{self, errno as mix_errno, Outcome, Request};
use audiomix::{Config, Mixer};
use user::messenger::audio::{self as api, wire};
use user::messenger::{services, Endpoint, Error as MsgError, Message, Parcel};
use user::sys;

use super::card::Card;
use super::ring::MappedRing;

/// Ticks between attempts to reach the card while there is none.
const CARD_RETRY_TICKS: u64 = 100;

/// A `Drain` waiting for its stream to play out.
struct Pending {
    stream: u32,
    txn: u64,
    /// The stream was stopped or closed first: answer `ECANCELED`.
    cancelled: bool,
}

pub(super) struct Server {
    /// Created with the first card and kept across card loss, so the master
    /// volume and the open streams survive a driver restart.
    mixer: Option<Mixer<MappedRing>>,
    card: Option<Card>,
    card_retry_at: u64,
    waiting_logged: bool,
    pending: Vec<Pending>,
    /// Streams whose drain finished this step (reused; never reallocated in
    /// the steady state).
    finished: Vec<u32>,
}

impl Server {
    pub(super) fn new() -> Server {
        Server {
            mixer: None,
            card: None,
            card_retry_at: 0,
            waiting_logged: false,
            pending: Vec::new(),
            finished: Vec::new(),
        }
    }

    pub(super) fn has_card(&self) -> bool {
        self.card.is_some()
    }

    /// Whether the loop should wake every tick: the card is playing or a
    /// stream is about to make it play.
    pub(super) fn busy(&self) -> bool {
        self.card.as_ref().is_some_and(Card::running)
            || self.mixer.as_ref().is_some_and(Mixer::wants_output)
    }

    /// Answer one request: `Some(reply)`, or `None` when a `Drain` is now
    /// pending (or the request expects no reply).
    pub(super) fn dispatch(&mut self, message: &Message) -> Option<Parcel> {
        let interface = message.interface_id();
        let method = message.method();
        // Only a method that declares a buffer in `audio.midl` adopts one.
        let wants_ring = interface == api::INTERFACE && wire::request_transfers(method).buffers > 0;
        let ring = take_ring(message, wants_ring);
        close_endpoint(message);
        let outcome = service::handle(
            self.mixer.as_mut(),
            Request {
                interface,
                method,
                body: &message.parcel.body,
                sender: message.sender,
                ring,
                now: sys::clock(),
            },
        );
        match outcome {
            Outcome::Reply(body) => {
                if interface == api::INTERFACE {
                    self.after_stream_call(method, &message.parcel.body);
                }
                Some(api::reply_parcel(interface, method, body))
            }
            Outcome::Refuse(code) => Some(error_parcel(interface, method, code)),
            Outcome::DrainPending(stream) => {
                if let Some(txn) = message.txn {
                    self.pending.push(Pending {
                        stream,
                        txn,
                        cancelled: false,
                    });
                }
                None
            }
        }
    }

    /// A successful `Stop` or `CloseStream` cancels a pending drain of the
    /// same stream (possible only from a second thread of the owner).
    fn after_stream_call(&mut self, method: u32, body: &[u8]) {
        let stream = match method {
            wire::METHOD_STOP => wire::decode_stop_args(body).map(|a| a.stream),
            wire::METHOD_CLOSESTREAM => wire::decode_close_stream_args(body).map(|a| a.stream),
            _ => return,
        };
        if let Ok(stream) = stream {
            self.cancel(stream);
        }
    }

    /// Mark a pending drain of `stream` as cancelled; replied in [`Server::step`].
    fn cancel(&mut self, stream: u32) {
        for pending in self.pending.iter_mut().filter(|p| p.stream == stream) {
            pending.cancelled = true;
        }
    }

    /// Everything that is not a request: reach the card, feed it, finish
    /// drains, reclaim abandoned streams.
    pub(super) fn step(&mut self, endpoint: &Endpoint, now: u64) {
        self.connect_card(now);
        self.finished.clear();
        if let (Some(card), Some(mixer)) = (self.card.as_mut(), self.mixer.as_mut()) {
            let finished = &mut self.finished;
            if let Err(error) = card.pump(mixer, now, |stream| finished.push(stream)) {
                sys::write_str(&format!("AUDIOD:CARD:LOST {error}\n"));
                if let Some(card) = self.card.take() {
                    card.close();
                }
                mixer.forget_card(|stream| finished.push(stream));
                self.card_retry_at = now + CARD_RETRY_TICKS;
            }
        }
        // A draining stream is never reclaimed, so no pending drain is lost.
        if let Some(mixer) = self.mixer.as_mut() {
            mixer.reclaim(now, |stream| {
                sys::write_str(&format!("AUDIOD:RECLAIM stream={stream}\n"))
            });
        }
        self.answer_drains(endpoint);
    }

    /// Reply to every pending drain that finished or was cancelled.
    fn answer_drains(&mut self, endpoint: &Endpoint) {
        let finished = &self.finished;
        let mut index = 0;
        while index < self.pending.len() {
            let pending = &self.pending[index];
            let reply = if pending.cancelled {
                Some(error_parcel(
                    api::INTERFACE,
                    wire::METHOD_DRAIN,
                    mix_errno::ECANCELED,
                ))
            } else if finished.contains(&pending.stream) {
                Some(api::reply_parcel(
                    api::INTERFACE,
                    wire::METHOD_DRAIN,
                    service::drained_reply(),
                ))
            } else {
                None
            };
            match reply {
                Some(reply) => {
                    let pending = self.pending.swap_remove(index);
                    let _ = endpoint.reply_or_drop(pending.txn, &reply);
                }
                None => index += 1,
            }
        }
    }

    /// Open the card when there is none and it is time to try.
    fn connect_card(&mut self, now: u64) {
        if self.card.is_some() || now < self.card_retry_at {
            return;
        }
        match Card::open(now) {
            Ok(card) => {
                sys::write_str(&format!(
                    "AUDIOD:CARD rate={} channels=2 period={}\n",
                    card.rate(),
                    card.period_frames()
                ));
                let config = Config::new(card.rate(), card.period_frames());
                let stale = self
                    .mixer
                    .as_ref()
                    .is_some_and(|m| m.config().rate != config.rate);
                if self.mixer.is_none() || stale {
                    self.mixer = Some(Mixer::new(config));
                }
                self.card = Some(card);
            }
            Err(error) => {
                if !self.waiting_logged {
                    sys::write_str(&format!("AUDIOD:WAITCARD {error}\n"));
                    self.waiting_logged = true;
                }
                self.card_retry_at = now + CARD_RETRY_TICKS;
            }
        }
    }
}

/// The error reply carrying `code` (a positive errno).
fn error_parcel(interface: u64, method: u32, code: i64) -> Parcel {
    services::error_reply(interface, method, MsgError::Errno(-code))
}

/// The request's ring, mapped, when it is an `AttachRing` that carried one.
/// Any buffer on another method is closed at once, so a client cannot fill
/// this task's handle table with transfers it never uses. The kernel surfaces
/// only the first buffer of a request.
fn take_ring(message: &Message, wanted: bool) -> Option<MappedRing> {
    if message.buffers == 0 {
        return None;
    }
    let handle = message.first_buffer;
    match message.parcel.buffers.first() {
        Some(desc) if wanted => MappedRing::map(handle, desc),
        _ => {
            let _ = sys::display_close_buffer(handle);
            None
        }
    }
}

/// No method takes an endpoint: close any that was transferred.
fn close_endpoint(message: &Message) {
    if message.handles > 0 {
        let _ = Endpoint::from_raw(message.first_handle).close();
    }
}
