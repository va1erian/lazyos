//! The driver engine: one attached client, its two frame rings, the frame
//! policy and the receive filter, pumped from the driver's loop.
//!
//! **Data path.** The driver copies between its own DMA slots and the client's
//! rings in both directions and trusts neither side (`docs/networking-plan.md`
//! section 5):
//!
//! * device to client: a completed receive buffer is copied out of its slot,
//!   checked (packet header plain, frame 14..=`max_frame` bytes), filtered by
//!   the receive mode and pushed into the client's receive ring;
//! * client to device: a frame is popped from the client's transmit ring into a
//!   private buffer, checked against the same length bounds and copied into a
//!   transmit slot at its full length. A frame outside the bounds is dropped
//!   and counted, never truncated, so it never reaches the device or the wire.
//!
//! **Hostile client.** A ring whose peer index is impossible is poisoned; the
//! engine detaches that client and counts a ring error. A second attach is
//! `Busy`, and only the kernel-stamped owner recorded at attach may change
//! anything afterwards.

use framering::{valid_slots, FrameBuf, PopError, Producer, PushError, Ring, MAX_FRAME};
use messenger_generated::os_lazy_net_nic_v1 as nic;
use virtio_net::frame::{classify, FrameClass, RxError};
use virtio_net::queue as qi;

use crate::queues::{Queues, TxError};
use crate::rings::NicRings;
use crate::stats::Stats;
use crate::{Doorbell, Fatal};

/// `Notify.events` bits (`NotifyBit` ordinals in `idl/net.midl`).
pub const EV_RX_READY: u32 = 1 << 0;
pub const EV_TX_SPACE: u32 = 1 << 1;
pub const EV_LINK_CHANGE: u32 = 1 << 2;

/// Client-ring frames examined per pump, so a producer that keeps publishing
/// cannot keep the driver in one call forever.
const TX_BUDGET: u32 = 4096;

/// Which received frames reach the client (`RxMode` ordinals).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RxMode {
    Off,
    /// Frames to this card's MAC, broadcast and multicast.
    Filtered,
    Promiscuous,
}

impl RxMode {
    pub fn from_u32(value: u32) -> Option<RxMode> {
        match value {
            0 => Some(RxMode::Off),
            1 => Some(RxMode::Filtered),
            2 => Some(RxMode::Promiscuous),
            _ => None,
        }
    }
}

/// Why an attach was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachError {
    /// A client is already attached.
    Busy,
    /// The slot count or the buffer is wrong.
    Invalid,
}

/// Why a control call on the attached ring was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CtlError {
    /// Nothing is attached, or the ring id is not the attached one.
    NoRing,
    /// The caller is not the owner.
    Denied,
}

/// What one [`Engine::pump`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PumpOutcome {
    /// `EV_*` bits the client should be told about (send one `Notify`).
    pub events: u32,
    /// The client was detached during this pump because its ring was corrupt.
    pub detached: bool,
    pub rx_delivered: u32,
    pub tx_sent: u32,
}

struct Session {
    owner: u64,
    ring: u32,
    /// The client's receive ring: the driver produces.
    rx: Producer,
    /// The client's transmit ring: the driver consumes.
    tx: framering::Consumer,
}

/// The engine over the card's rings `R` (virtio-net's [`Queues`] unless
/// another card says otherwise).
pub struct Engine<R: NicRings = Queues> {
    queues: R,
    mac: [u8; 6],
    max_frame: usize,
    link: bool,
    link_event: bool,
    mode: RxMode,
    session: Option<Session>,
    stats: Stats,
    next_ring: u32,
    /// Private copy of the frame popped from the client's transmit ring.
    tx_buf: FrameBuf,
}

impl<R: NicRings> Engine<R> {
    /// `max_frame` is the MTU plus the Ethernet header.
    pub fn new(queues: R, mac: [u8; 6], max_frame: usize, link: bool) -> Engine<R> {
        Engine {
            queues,
            mac,
            max_frame: max_frame.min(virtio_net::MAX_FRAME).min(MAX_FRAME),
            link,
            link_event: false,
            mode: RxMode::Filtered,
            session: None,
            stats: Stats::default(),
            next_ring: 1,
            tx_buf: [0; MAX_FRAME],
        }
    }

    pub fn queues(&self) -> &R {
        &self.queues
    }

    pub fn queues_mut(&mut self) -> &mut R {
        &mut self.queues
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    pub fn max_frame(&self) -> usize {
        self.max_frame
    }

    pub fn link(&self) -> bool {
        self.link
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Apply a new MTU (a live setting): frames longer than `mtu` plus the
    /// Ethernet header are refused from now on, in both directions.
    pub fn set_max_frame(&mut self, max_frame: usize) {
        self.max_frame = max_frame.min(virtio_net::MAX_FRAME).min(MAX_FRAME);
    }

    pub fn count_interrupt(&mut self) {
        self.stats.interrupts += 1;
    }

    pub fn rx_mode(&self) -> RxMode {
        self.mode
    }

    /// The attached client's owner and ring id.
    pub fn attached(&self) -> Option<(u64, u32)> {
        self.session.as_ref().map(|s| (s.owner, s.ring))
    }

    /// Record a link state read from the device. Returns whether it changed;
    /// an attached client is told on the next pump.
    pub fn set_link(&mut self, up: bool) -> bool {
        if up == self.link {
            return false;
        }
        self.link = up;
        self.stats.link_changes = self.stats.link_changes.wrapping_add(1);
        self.link_event = true;
        true
    }

    /// Attach `owner`'s rings: `len` bytes at `base` holding the receive and
    /// transmit rings as `AttachRing` declares them (`attach_ring_rings`), each
    /// `ring_bytes(slots)` long. Returns the ring
    /// id. The rings must have been created by the client (`Ring::create`); a
    /// header that is wrong, or rings that are not exactly the size `slots`
    /// implies, are refused.
    ///
    /// # Safety
    /// `base` must be valid for reads and writes of `len` bytes and stay
    /// mapped until the client is detached (the engine reports detaches through
    /// [`PumpOutcome::detached`], [`Engine::detach`] and [`Engine::release`]);
    /// the client may modify it at any time.
    pub unsafe fn attach(
        &mut self,
        owner: u64,
        slots: u32,
        base: *mut u8,
        len: usize,
    ) -> Result<u32, AttachError> {
        if self.session.is_some() {
            return Err(AttachError::Busy);
        }
        if !valid_slots(slots) {
            return Err(AttachError::Invalid);
        }
        let one = framering::ring_bytes(slots);
        // The layout comes from `AttachRing`'s `Ring<Rx, Tx>` declaration in
        // `idl/net.midl`, the same function the client lays its buffer out with.
        let layout = nic::attach_ring_rings(one as u64).ok_or(AttachError::Invalid)?;
        if len as u64 != layout.total {
            return Err(AttachError::Invalid);
        }
        let (rx_at, tx_at) = (layout.rx as usize, layout.tx as usize);
        // SAFETY: the layout's offsets and `one`-byte rings lie inside
        // `layout.total == len` bytes, which the caller vouches for.
        let (rx, tx) = unsafe {
            (
                Ring::attach(base.add(rx_at), one, slots),
                Ring::attach(base.add(tx_at), one, slots),
            )
        };
        let (Ok(rx), Ok(tx)) = (rx, tx) else {
            return Err(AttachError::Invalid);
        };
        let ring = self.next_ring;
        self.next_ring = self.next_ring.checked_add(1).unwrap_or(1);
        let mut session = Session {
            owner,
            ring,
            rx: rx.producer(),
            tx: tx.consumer(),
        };
        // Ask the client to kick us when it queues frames.
        session.tx.arm();
        self.session = Some(session);
        Ok(ring)
    }

    fn owned(&self, owner: u64, ring: u32) -> Result<&Session, CtlError> {
        let session = self.session.as_ref().ok_or(CtlError::NoRing)?;
        if session.owner != owner {
            return Err(CtlError::Denied);
        }
        if session.ring != ring {
            return Err(CtlError::NoRing);
        }
        Ok(session)
    }

    /// `DetachRing`: release the client's rings.
    pub fn detach(&mut self, owner: u64, ring: u32) -> Result<(), CtlError> {
        self.owned(owner, ring)?;
        self.session = None;
        Ok(())
    }

    /// Drop whatever is attached (the owner exited, or its notify endpoint
    /// reported the peer gone).
    pub fn release(&mut self) {
        self.session = None;
    }

    /// `Kick`: whether it came from the owner for the attached ring. A kick
    /// from anyone else, or for another ring, is ignored by the caller.
    pub fn kick(&self, sender: u64, ring: u32) -> bool {
        self.owned(sender, ring).is_ok()
    }

    /// `SetRxMode`: only the owner, only known modes (`None` for an unknown
    /// mode, which the caller answers `EINVAL`).
    pub fn set_rx_mode(&mut self, owner: u64, mode: u32) -> Result<Option<RxMode>, CtlError> {
        let session = self.session.as_ref().ok_or(CtlError::NoRing)?;
        if session.owner != owner {
            return Err(CtlError::Denied);
        }
        let Some(mode) = RxMode::from_u32(mode) else {
            return Ok(None);
        };
        self.mode = mode;
        Ok(Some(mode))
    }

    /// Whether a received frame passes the receive mode.
    fn accepts(mode: RxMode, mac: &[u8; 6], frame: &[u8]) -> bool {
        match mode {
            RxMode::Off => false,
            RxMode::Promiscuous => true,
            // Destination MAC: ours, or a group address (broadcast included).
            RxMode::Filtered => frame[0] & 1 != 0 || frame[..6] == mac[..],
        }
    }

    /// Do all pending work: reap finished transmits, move received frames to
    /// the client, move the client's queued frames to the device, and kick the
    /// device once per queue that gained buffers.
    pub fn pump(&mut self, bell: &mut impl Doorbell) -> Result<PumpOutcome, Fatal> {
        let mut out = PumpOutcome::default();
        self.queues.reap_tx()?;
        self.pump_rx(bell, &mut out)?;
        self.pump_tx(bell, &mut out);
        if self.link_event && self.session.is_some() {
            out.events |= EV_LINK_CHANGE;
        }
        self.link_event = false;
        Ok(out)
    }

    fn pump_rx(&mut self, bell: &mut impl Doorbell, out: &mut PumpOutcome) -> Result<(), Fatal> {
        let Engine {
            queues,
            session,
            stats,
            mode,
            mac,
            max_frame,
            ..
        } = self;
        let mut delivered = 0u32;
        let mut detached = false;
        let handled = queues.poll_frames(*max_frame, |received| {
            let frame = match received {
                Ok(frame) => frame,
                Err(error) => {
                    stats.rx_dropped += 1;
                    match error {
                        RxError::Runt => stats.runts += 1,
                        RxError::Oversize => stats.oversize += 1,
                        // The device broke the contract for this buffer.
                        RxError::NoHeader | RxError::Overrun | RxError::NotPlain => {
                            stats.ring_errors += 1
                        }
                    }
                    return;
                }
            };
            if !Self::accepts(*mode, mac, frame) {
                stats.rx_dropped += 1;
                return;
            }
            let Some(client) = session.as_mut() else {
                stats.rx_dropped += 1;
                return;
            };
            match client.rx.push(frame) {
                Ok(()) => {
                    stats.rx_frames += 1;
                    stats.rx_bytes += frame.len() as u64;
                    delivered += 1;
                }
                // A client that does not drain its ring loses frames, not us.
                Err(PushError::Full) => stats.rx_dropped += 1,
                Err(PushError::Corrupt) => {
                    stats.rx_dropped += 1;
                    stats.ring_errors += 1;
                    *session = None;
                    detached = true;
                }
                // `rx_frame` only returns 14..=max_frame bytes, which the ring accepts.
                Err(PushError::Empty | PushError::TooLong) => stats.rx_dropped += 1,
            }
        })?;
        if handled > 0 {
            bell.ring(qi::RX);
        }
        out.rx_delivered = delivered;
        out.detached |= detached;
        if delivered > 0 {
            if let Some(client) = session.as_mut() {
                if client.rx.take_notify() {
                    out.events |= EV_RX_READY;
                }
            }
        }
        Ok(())
    }

    fn pump_tx(&mut self, bell: &mut impl Doorbell, out: &mut PumpOutcome) {
        let Engine {
            queues,
            session,
            stats,
            max_frame,
            tx_buf,
            ..
        } = self;
        let Some(client) = session.as_mut() else {
            return;
        };
        let mut poisoned = false;
        let was_full = match client.tx.pending() {
            Ok(pending) => pending >= client.tx.slots(),
            Err(_) => {
                poisoned = true;
                false
            }
        };
        let mut consumed = 0u32;
        let mut budget = TX_BUDGET;
        while !poisoned && budget > 0 && queues.tx_free() > 0 {
            budget -= 1;
            match client.tx.pop(tx_buf) {
                Ok(Some(len)) => {
                    consumed += 1;
                    match classify(len, *max_frame) {
                        FrameClass::Runt => {
                            stats.runts += 1;
                            stats.tx_dropped += 1;
                        }
                        FrameClass::Oversize => {
                            stats.oversize += 1;
                            stats.tx_dropped += 1;
                        }
                        FrameClass::Ok => match queues.tx_send(&tx_buf[..len]) {
                            Ok(()) => {
                                stats.tx_frames += 1;
                                stats.tx_bytes += len as u64;
                                out.tx_sent += 1;
                            }
                            Err(TxError::NoSlot | TxError::TooLong) => stats.tx_dropped += 1,
                        },
                    }
                }
                Ok(None) => {
                    // Looked, found nothing: ask for a kick, then look once
                    // more so a frame published in between is not missed.
                    client.tx.arm();
                    match client.tx.pending() {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(_) => poisoned = true,
                    }
                }
                Err(PopError::BadLength(_)) => {
                    consumed += 1;
                    stats.oversize += 1;
                    stats.tx_dropped += 1;
                }
                Err(PopError::Corrupt) => poisoned = true,
            }
        }
        if out.tx_sent > 0 {
            bell.ring(qi::TX);
        }
        if was_full && consumed > 0 {
            out.events |= EV_TX_SPACE;
        }
        if poisoned {
            stats.ring_errors += 1;
            *session = None;
            out.detached = true;
        }
    }
}
