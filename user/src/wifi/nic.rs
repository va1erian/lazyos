//! `os.lazy.net.nic.v1` for a radio: the soft-MAC's data path behind the
//! driver engine `netdrv` uses (`libs/nicdrv`), so `netd` sees one more card
//! (`wlan0`, kind wireless) with the same rings, the same frame policy and
//! the same hostile-client rules.
//!
//! [`MacRings`] is the engine's "card": receive drains the Ethernet frames
//! the soft-MAC decapsulated, transmit hands each frame to
//! [`SoftMac::send_ethernet`]. The request dispatch follows `netdrv`'s
//! (`user/src/bin/netdrv/service.rs`): one client, owned by the
//! kernel-stamped sender of `AttachRing`.

use alloc::format;
use alloc::vec::Vec;

use nicdrv::engine::{AttachError, CtlError};
use nicdrv::{Doorbell, Engine, Fatal, NicRings, RxError, TxError};
use wifimac::{DataDrop, Radio, SoftMac};

use crate::messenger::net::{self as api, wire};
use crate::messenger::{errno, Endpoint, Error as MsgError, Message, Parcel};
use crate::sys;

/// Frames moved to the client per pump.
const RX_BUDGET: u32 = 64;
/// Transmit room reported while the radio takes frames.
const TX_ROOM: u16 = 32;
/// Milliseconds between empty `Notify` messages: the liveness probe that
/// notices a client that exited without detaching.
const PROBE_MS: u64 = 1000;

/// The soft-MAC as the engine's card.
pub struct MacRings<R: Radio> {
    pub mac: SoftMac<R>,
}

impl<R: Radio> NicRings for MacRings<R> {
    fn poll_frames(
        &mut self,
        max_frame: usize,
        mut deliver: impl FnMut(Result<&[u8], RxError>),
    ) -> Result<u32, Fatal> {
        let mut handled = 0;
        while handled < RX_BUDGET {
            let Some(frame) = self.mac.next_ethernet() else {
                break;
            };
            handled += 1;
            if frame.len() < virtio_net::ETH_HEADER {
                deliver(Err(RxError::Runt));
            } else if frame.len() > max_frame {
                deliver(Err(RxError::Oversize));
            } else {
                deliver(Ok(&frame));
            }
        }
        Ok(handled)
    }

    fn tx_free(&self) -> u16 {
        if self.mac.tx_ready() {
            TX_ROOM
        } else {
            0
        }
    }

    fn tx_send(&mut self, frame: &[u8]) -> Result<(), TxError> {
        match self.mac.send_ethernet(frame) {
            Ok(()) => Ok(()),
            Err(DataDrop::TooLong) => Err(TxError::TooLong),
            Err(_) => Err(TxError::NoSlot),
        }
    }

    fn reap_tx(&mut self) -> Result<u16, Fatal> {
        Ok(0)
    }

    fn rx_in_flight(&self) -> u16 {
        0
    }
}

/// The soft-MAC has no device doorbell: the radio is pumped by the loop.
pub struct NoBell;

impl Doorbell for NoBell {
    fn ring(&mut self, _queue: u16) {}
}

fn err(code: i64) -> MsgError {
    MsgError::Errno(-code)
}

/// The attached client's shared buffer and notify endpoint.
struct Attachment {
    buffer: u64,
    notify: Endpoint,
    next_probe: u64,
}

/// The card side: the engine (which owns the soft-MAC) and the attachment.
pub struct NicService<R: Radio> {
    pub engine: Engine<MacRings<R>>,
    attachment: Option<Attachment>,
    ifname: alloc::string::String,
}

impl<R: Radio> NicService<R> {
    pub fn new(mac: SoftMac<R>, ifname: &str) -> NicService<R> {
        let max_frame = usize::from(mac.mtu()) + virtio_net::ETH_HEADER;
        let station = mac.info().mac;
        NicService {
            engine: Engine::new(MacRings { mac }, station, max_frame, false),
            attachment: None,
            ifname: alloc::string::String::from(ifname),
        }
    }

    pub fn mac(&mut self) -> &mut SoftMac<R> {
        &mut self.engine.queues_mut().mac
    }

    fn drop_attachment(&mut self, why: &str) {
        if let Some(attachment) = self.attachment.take() {
            sys::write_str(&format!("WIFI:NIC:DETACH if={} {why}\n", self.ifname));
            let _ = sys::buffer_close(attachment.buffer);
            let _ = attachment.notify.release();
        }
    }

    fn sync(&mut self) {
        if self.attachment.is_some() && self.engine.attached().is_none() {
            self.drop_attachment("ring corrupt");
        }
    }

    fn notify(&mut self, events: u32) {
        let Some(ring) = self.engine.attached().map(|(_, ring)| ring) else {
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
                self.engine.release();
                self.drop_attachment("client gone");
            }
        }
    }

    /// Follow the soft-MAC's link, move frames both ways, tell the client.
    /// Returns whether the link changed (the caller republishes its topic).
    pub fn pump(&mut self, now_ms: u64) -> Result<bool, Fatal> {
        let up = self.engine.queues().mac.link_up();
        let changed = self.engine.set_link(up);
        let outcome = self.engine.pump(&mut NoBell)?;
        self.sync();
        if outcome.events != 0 {
            self.notify(outcome.events);
        }
        if let Some(attachment) = self.attachment.as_mut() {
            if now_ms >= attachment.next_probe {
                attachment.next_probe = now_ms + PROBE_MS;
                if outcome.events == 0 {
                    self.notify(0);
                }
            }
        }
        Ok(changed)
    }

    /// Answer one request on the card's endpoint.
    pub fn dispatch(&mut self, message: &Message) -> Result<Parcel, MsgError> {
        let result = self.route(message);
        self.sync();
        result
    }

    fn route(&mut self, message: &Message) -> Result<Parcel, MsgError> {
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
                match self.engine.set_rx_mode(message.sender, args.mode) {
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
                self.engine
                    .detach(message.sender, args.ring)
                    .map_err(ctl_errno)?;
                self.drop_attachment("detached by its owner");
                Vec::new()
            }
            wire::METHOD_KICK => {
                // One-way: frames move in `pump`, which runs after this.
                if let Ok(args) = wire::decode_kick_args(body) {
                    let _ = self.engine.kick(message.sender, args.ring);
                }
                Vec::new()
            }
            _ => return Err(err(errno::EINVAL)),
        };
        Ok(api::parcel(method, reply, Vec::new()))
    }

    fn info(&self) -> Result<Vec<u8>, MsgError> {
        let engine = &self.engine;
        wire::encode_info_reply(&wire::InfoReply {
            info: wire::NicInfo {
                mac: engine.mac().to_vec(),
                mtu: u32::from(engine.queues().mac.mtu()),
                max_frame: engine.max_frame() as u32,
                link: engine.link(),
                features: 0,
                kind: wire::NIC_KIND_WIRELESS,
            },
        })
        .map_err(MsgError::Parcel)
    }

    fn stats(&self) -> Result<Vec<u8>, MsgError> {
        let s = self.engine.stats();
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

    /// `AttachRing`: the decoded request owns the ring buffer and the notify
    /// endpoint; every way out but success closes them.
    fn attach(&mut self, message: &Message, args: &wire::AttachRingArgs) -> Result<u32, MsgError> {
        let outcome = self.attach_inner(message, args);
        if outcome.is_err() {
            let _ = sys::buffer_close(args.rings.handle);
            let _ = Endpoint::from_raw(args.notify).release();
        }
        outcome
    }

    fn attach_inner(&mut self, message: &Message, args: &wire::AttachRingArgs) -> Result<u32, MsgError> {
        if self.engine.attached().is_some() {
            return Err(err(errno::EBUSY));
        }
        let (va, size) = sys::buffer_map(args.rings.handle).map_err(MsgError::Errno)?;
        if !args.rings.fits(size) {
            return Err(err(errno::EINVAL));
        }
        let offset = usize::try_from(args.rings.offset).map_err(|_| err(errno::EINVAL))?;
        let len = usize::try_from(args.rings.len).map_err(|_| err(errno::EINVAL))?;
        let base = (va as usize)
            .checked_add(offset)
            .ok_or_else(|| err(errno::EINVAL))?;
        // SAFETY: `offset..offset + len` lies inside the `size` bytes the
        // kernel mapped at `va` (`fits` checked it); the mapping stays until
        // `drop_attachment` closes the buffer, which happens whenever the
        // engine lets the client go. The client may rewrite the memory at any
        // time, which the ring implementation tolerates.
        let attached = unsafe {
            self.engine
                .attach(message.sender, args.slots, base as *mut u8, len)
        };
        match attached {
            Ok(ring) => {
                self.attachment = Some(Attachment {
                    buffer: args.rings.handle,
                    notify: Endpoint::from_raw(args.notify),
                    next_probe: sys::monotonic_ms() + PROBE_MS,
                });
                sys::write_str(&format!(
                    "WIFI:NIC:ATTACH if={} owner={} ring={ring}\n",
                    self.ifname, message.sender
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
