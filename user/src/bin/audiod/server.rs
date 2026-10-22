//! Requests in, replies out: the Messenger side of the mixer.
//!
//! Decoding and the stream rules live in `audiomix::service`, shared with the
//! host tests; this module adds what only a real server has: transferred
//! handles, deferred `Drain` replies, the card connection and the timers.
//!
//! The card is one of the outputs drivers register under
//! `os.lazy.audio.card/<id>` (`audiomix::outputs` decides which): the
//! registry is scanned once a second, and when the output to use changes
//! (a headset plugged in or out, `SelectOutput`) the new card is opened
//! before the old one is let go, and the open streams carry on through it.

use alloc::string::String;
use alloc::vec::Vec;
use user::trace;

use audiomix::outputs::{card_id, Outputs};
use audiomix::service::{self, errno as mix_errno, Outcome, Request};
use audiomix::{Config, Mixer};
use user::messenger::audio::{self as api, wire};
use user::messenger::{registry, services, Endpoint, Error as MsgError, Message, Parcel};
use user::sys;

use super::card::Card;
use super::ring::MappedRing;

/// Ticks between attempts to reach the card while there is none.
const CARD_RETRY_TICKS: u64 = 100;
/// Ticks between looks at which outputs are registered.
const SCAN_TICKS: u64 = 100;

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
    /// The output `card` is.
    card_id: Option<String>,
    card_retry_at: u64,
    waiting_logged: bool,
    outputs: Outputs,
    scanned_at: Option<u64>,
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
            card_id: None,
            card_retry_at: 0,
            waiting_logged: false,
            outputs: Outputs::new(),
            scanned_at: None,
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
        // Only `AttachRing` carries an object, its `Ring<Samples>` (the
        // kernel gate holds every request to its declaration); the message
        // keeps whatever it carried until the ring is taken, and closes it
        // when it drops.
        // Which output plays is `audiod`'s, not the engine's.
        if interface == api::CONTROL_INTERFACE {
            if let Some(outcome) = self.outputs.handle(method, &message.parcel.body) {
                if matches!(outcome, Outcome::Reply(_)) && method == api::control_wire::METHOD_SELECTOUTPUT {
                    // Switch on the next step, whatever the retry timer says.
                    self.card_retry_at = 0;
                }
                return Some(match outcome {
                    Outcome::Reply(body) => api::reply_parcel(interface, method, body),
                    Outcome::Refuse(code) => error_parcel(interface, method, code),
                    Outcome::DrainPending(_) => error_parcel(interface, method, mix_errno::EINVAL),
                });
            }
        }
        let ring = if interface == api::INTERFACE && method == wire::METHOD_ATTACHRING {
            match message.decode(wire::decode_attach_ring_args) {
                Ok(args) => MappedRing::map(&args.ring),
                Err(_) => return Some(error_parcel(interface, method, mix_errno::EINVAL)),
            }
        } else {
            None
        };
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
        self.finished.clear();
        self.connect_card(now);
        if let (Some(card), Some(mixer)) = (self.card.as_mut(), self.mixer.as_mut()) {
            let finished = &mut self.finished;
            if let Err(error) = card.pump(mixer, now, |stream| finished.push(stream)) {
                trace::line!(trace::Channel::Audiod, "CARD:LOST {error}");
                if let Some(card) = self.card.take() {
                    card.close();
                }
                mixer.forget_card(|stream| finished.push(stream));
                self.card_id = None;
                self.outputs.set_active(None);
                // Look again at once: the next output may already be there.
                self.scanned_at = None;
                self.card_retry_at = now + CARD_RETRY_TICKS / 10;
            }
        }
        // A draining stream is never reclaimed, so no pending drain is lost.
        if let Some(mixer) = self.mixer.as_mut() {
            mixer.reclaim(now, |stream| {
                trace::line!(trace::Channel::Audiod, "RECLAIM stream={stream}")
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

    /// Hand the streams and this step's finished drains to `watch` (issue
    /// #453); the events it finds land in `out`.
    pub(super) fn observe(
        &self,
        watch: &mut audiomix::events::Watch,
        now: u64,
        periods: bool,
        out: &mut Vec<audiomix::events::Event>,
    ) {
        if let Some(mixer) = self.mixer.as_ref() {
            watch.step(mixer.statuses(), &self.finished, now, periods, out);
        }
    }

    /// Find the outputs, and play through the one to use: open it when
    /// there is no card or another output should play.
    fn connect_card(&mut self, now: u64) {
        if self.scanned_at.is_none_or(|at| now >= at + SCAN_TICKS) {
            self.scanned_at = Some(now);
            self.scan();
        }
        let Some(target) = self.outputs.target().map(String::from) else {
            if !self.waiting_logged {
                trace::line!(trace::Channel::Audiod, "WAITCARD no output registered");
                self.waiting_logged = true;
            }
            return;
        };
        if self.card.is_some() && self.card_id.as_deref() == Some(target.as_str()) {
            return;
        }
        if now < self.card_retry_at {
            return;
        }
        let opening = sys::clock();
        match Card::open(&target, now) {
            Ok(card) => {
                let ticks = sys::clock() - opening;
                if ticks > 10 {
                    trace::line!(trace::Channel::Audiod, "CARD:SLOW id={target} open_ticks={ticks}");
                }
                self.adopt(target, card)
            }
            Err(error) => {
                trace::line!(trace::Channel::Audiod, "CARD:FAIL id={target} {error}");
                // Passed over for a while: the next output plays meanwhile.
                self.outputs.failed(&target);
                self.card_retry_at = now + CARD_RETRY_TICKS;
            }
        }
    }

    /// Play through `card` (output `id`) from now on; the card it replaces
    /// is let go, and the streams are resampled to the new rate.
    fn adopt(&mut self, id: String, card: Card) {
        let finished = &mut self.finished;
        let started = sys::clock();
        if let Some(old) = self.card.take() {
            old.close();
            if let Some(mixer) = self.mixer.as_mut() {
                mixer.forget_card(|stream| finished.push(stream));
            }
        }
        trace::line!(
            trace::Channel::Audiod,
            "CARD id={id} rate={} channels=2 period={} close_ticks={} name={}",
            card.rate(),
            card.period_frames(),
            sys::clock() - started,
            card.name()
        );
        match self.mixer.as_mut() {
            Some(mixer) => mixer.retarget(card.rate(), card.period_frames()),
            None => self.mixer = Some(Mixer::new(Config::new(card.rate(), card.period_frames()))),
        }
        if let Some(mixer) = self.mixer.as_mut() {
            mixer.set_output_name(card.name());
        }
        self.outputs.set_name(&id, card.name());
        self.outputs.set_active(Some(&id));
        self.card_id = Some(id);
        self.card = Some(card);
        self.waiting_logged = false;
    }

    /// The outputs registered now: cards whose name a sound driver owns
    /// (`_snd`, or root on a kernel-spawned boot).
    fn scan(&mut self) {
        let Ok(entries) = registry::list() else {
            return;
        };
        let ids: Vec<String> = entries
            .iter()
            .filter(|entry| entry.owner_uid == u64::from(sndpolicy::SND_UID) || entry.owner_uid == 0)
            .filter_map(|entry| card_id(&entry.name).map(String::from))
            .collect();
        if self.outputs.update(ids.iter().map(String::as_str)) {
            trace::line!(trace::Channel::Audiod, "OUTPUTS {}", ids.join(","));
        }
    }
}

/// The error reply carrying `code` (a positive errno).
fn error_parcel(interface: u64, method: u32, code: i64) -> Parcel {
    services::error_reply(interface, method, MsgError::Errno(-code))
}
