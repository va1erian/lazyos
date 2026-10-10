//! `os.lazy.net.nic.v1` (`idl/net.midl`): request dispatch for the card.
//!
//! One card, one attached client. The owner is the kernel-stamped sender of
//! `AttachRing`; nothing in a request body names a caller, so a client cannot
//! act for another. Every failure leaves through
//! [`user::messenger::services::error_reply`] as a structured errno. The
//! frames themselves move in `Service::housekeeping` (through the driver
//! core), not in request handlers: a request only changes who is attached and
//! how.

use alloc::format;
use alloc::vec::Vec;

use nicdrv::engine::{AttachError, CtlError};
use user::messenger::net::{self as api, wire};
use user::messenger::{errno, Endpoint, Error as MsgError, Message, Parcel};
use user::sys;

use super::card::Card;
use super::error::Error;

type Result<T> = core::result::Result<T, MsgError>;

/// Ticks between empty `Notify` messages to an attached client: a liveness
/// probe that notices a client which exited without detaching.
const PROBE_TICKS: u64 = 100;

fn err(code: i64) -> MsgError {
    MsgError::Errno(-code)
}

/// The attached client's shared buffer (mapped for the engine) and the
/// endpoint notices go to; who owns it and which ring is the engine's record.
struct Attachment {
    buffer: u64,
    notify: Endpoint,
    next_probe: u64,
}

pub(super) struct Service {
    pub(super) card: Card,
    attachment: Option<Attachment>,
}

impl Service {
    pub(super) fn new(card: Card) -> Service {
        Service {
            card,
            attachment: None,
        }
    }

    /// Release the attachment's buffer and endpoint.
    fn drop_attachment(&mut self, why: &str) {
        if let Some(attachment) = self.attachment.take() {
            sys::write_str(&format!("NETDRV:DETACH {why}\n"));
            let _ = sys::buffer_close(attachment.buffer);
            let _ = attachment.notify.release();
        }
    }

    /// Keep the service and the engine in step: when the engine let the client
    /// go (a corrupt ring), its buffer and endpoint go too.
    fn sync(&mut self) {
        if self.attachment.is_some() && self.card.engine.attached().is_none() {
            self.drop_attachment("ring corrupt");
        }
    }

    /// Tell the client something happened. A send that fails because the
    /// endpoint is dead releases the attachment; a full inbox only loses this
    /// (coalesced) notice.
    fn notify(&mut self, events: u32) {
        let Some(ring) = self.card.engine.attached().map(|(_, ring)| ring) else {
            return;
        };
        let Some(attachment) = self.attachment.as_ref() else {
            return;
        };
        let Ok(body) = wire::encode_notify_args(&wire::NotifyArgs { ring, events }) else {
            return;
        };
        if let Err(error) = attachment
            .notify
            .send(&api::event(wire::METHOD_NOTIFY, body))
        {
            if matches!(error.errno(), Some(code) if code == -errno::EPIPE || code == -9) {
                self.card.engine.release();
                self.drop_attachment("client gone");
            }
        }
    }

    /// Do one round of work: link, frames, notices. Called on every wakeup,
    /// requested or not.
    pub(super) fn housekeeping(&mut self) -> core::result::Result<(), Error> {
        let now = sys::clock();
        self.card.poll_link(now);
        let outcome = self.card.pump()?;
        self.sync();
        if outcome.events != 0 {
            self.notify(outcome.events);
        }
        if let Some(attachment) = self.attachment.as_mut() {
            if now >= attachment.next_probe {
                attachment.next_probe = now + PROBE_TICKS;
                if outcome.events == 0 {
                    self.notify(0);
                }
            }
        }
        Ok(())
    }

    /// Route one request. `Ok` is the reply; `Err` becomes the error reply.
    /// The objects a request carried (`AttachRing`'s ring buffer and notify
    /// endpoint) are the message's until its decoder claims them: a refused
    /// request's objects close when the message drops.
    pub(super) fn dispatch(&mut self, message: &Message) -> Result<Parcel> {
        let result = self.route(message);
        self.sync();
        result
    }

    fn route(&mut self, message: &Message) -> Result<Parcel> {
        if message.interface_id() != api::INTERFACE {
            return Err(err(errno::EINVAL));
        }
        let method = message.method();
        let body = &message.parcel.body;
        let reply = match method {
            wire::METHOD_INFO => self.info()?,
            wire::METHOD_STATS => self.stats()?,
            wire::METHOD_SETRXMODE => {
                let args = wire::decode_set_rx_mode_args(body).map_err(MsgError::Parcel)?;
                match self.card.engine.set_rx_mode(message.sender, args.mode) {
                    Ok(Some(_)) => {}
                    Ok(None) => return Err(err(errno::EINVAL)),
                    Err(error) => return Err(ctl_errno(error)),
                }
                wire::encode_set_rx_mode_reply(&wire::SetRxModeReply { ok: true })
                    .map_err(MsgError::Parcel)?
            }
            wire::METHOD_ATTACHRING => {
                let args = message.decode(wire::decode_attach_ring_args)?;
                let ring = self.attach(message, &args)?;
                wire::encode_attach_ring_reply(&wire::AttachRingReply { ring })
                    .map_err(MsgError::Parcel)?
            }
            wire::METHOD_DETACHRING => {
                let args = wire::decode_detach_ring_args(body).map_err(MsgError::Parcel)?;
                self.card
                    .engine
                    .detach(message.sender, args.ring)
                    .map_err(ctl_errno)?;
                self.drop_attachment("detached by its owner");
                Vec::new()
            }
            wire::METHOD_KICK => {
                // One-way: a kick from anyone but the owner, or for another
                // ring, is ignored. The frames move in `housekeeping`, which
                // runs after this request.
                if let Ok(args) = wire::decode_kick_args(body) {
                    let _ = self.card.engine.kick(message.sender, args.ring);
                }
                Vec::new()
            }
            _ => return Err(err(errno::EINVAL)),
        };
        Ok(api::parcel(method, reply, Vec::new()))
    }

    fn info(&self) -> Result<Vec<u8>> {
        let engine = &self.card.engine;
        wire::encode_info_reply(&wire::InfoReply {
            info: wire::NicInfo {
                mac: engine.mac().to_vec(),
                mtu: u32::from(self.card.mtu),
                max_frame: engine.max_frame() as u32,
                link: engine.link(),
                features: 0,
                // Every card this driver serves is a cable.
                kind: wire::NIC_KIND_WIRED,
            },
        })
        .map_err(MsgError::Parcel)
    }

    fn stats(&self) -> Result<Vec<u8>> {
        let s = self.card.engine.stats();
        wire::encode_stats_reply(&wire::StatsReply {
            stats: wire::NicStats {
                rx_frames: s.rx_frames,
                tx_frames: s.tx_frames,
                rx_bytes: s.rx_bytes,
                tx_bytes: s.tx_bytes,
                rx_dropped: s.rx_dropped,
                tx_dropped: s.tx_dropped,
                runts: s.runts,
                oversize: s.oversize,
                ring_errors: s.ring_errors,
                interrupts: s.interrupts,
                link_changes: s.link_changes,
            },
        })
        .map_err(MsgError::Parcel)
    }

    /// `AttachRing`: map the client's buffer and hand it to the engine. The
    /// decoded request owns the ring buffer and the notify endpoint; every
    /// way out but success closes them.
    fn attach(&mut self, message: &Message, args: &wire::AttachRingArgs) -> Result<u32> {
        let outcome = self.attach_inner(message, args);
        if outcome.is_err() {
            let _ = sys::buffer_close(args.rings.handle);
            let _ = Endpoint::from_raw(args.notify).release();
        }
        outcome
    }

    fn attach_inner(&mut self, message: &Message, args: &wire::AttachRingArgs) -> Result<u32> {
        if self.card.engine.attached().is_some() {
            return Err(err(errno::EBUSY));
        }
        let (va, size) = sys::buffer_map(args.rings.handle).map_err(MsgError::Errno)?;
        // The range is the client's claim; the mapped size is the truth.
        if !args.rings.fits(size) {
            return Err(err(errno::EINVAL));
        }
        let offset = usize::try_from(args.rings.offset).map_err(|_| err(errno::EINVAL))?;
        let len = usize::try_from(args.rings.len).map_err(|_| err(errno::EINVAL))?;
        let base = (va as usize)
            .checked_add(offset)
            .ok_or_else(|| err(errno::EINVAL))?;
        // SAFETY: `offset..offset + len` lies inside the `size` bytes the
        // kernel mapped at `va` (checked above); the mapping stays until
        // `drop_attachment` closes the buffer, which happens whenever the
        // engine lets the client go. The client may rewrite the memory at any
        // time, which the ring implementation is built to tolerate.
        let attached = unsafe {
            self.card
                .engine
                .attach(message.sender, args.slots, base as *mut u8, len)
        };
        let slots = args.slots;
        match attached {
            Ok(ring) => {
                self.attachment = Some(Attachment {
                    buffer: args.rings.handle,
                    notify: Endpoint::from_raw(args.notify),
                    next_probe: sys::clock() + PROBE_TICKS,
                });
                sys::write_str(&format!(
                    "NETDRV:ATTACH owner={} ring={ring} slots={slots}\n",
                    message.sender
                ));
                Ok(ring)
            }
            Err(AttachError::Busy) => Err(err(errno::EBUSY)),
            Err(AttachError::Invalid) => Err(err(errno::EINVAL)),
        }
    }
}

fn ctl_errno(error: CtlError) -> MsgError {
    err(match error {
        CtlError::Denied => errno::EACCES,
        CtlError::NoRing => errno::EINVAL,
    })
}
