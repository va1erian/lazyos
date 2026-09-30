//! A smoltcp [`Device`] over the two frame rings shared with the NIC driver.
//!
//! The receive ring is consumed and the transmit ring produced. The driver is
//! a different (more trusted, but still separate) process, so the ring rules of
//! `libs/framering` apply: every frame is **copied out of the ring into a
//! private buffer before smoltcp parses it**, an impossible index poisons the
//! ring (and this device reports nothing more until the caller re-attaches), and
//! a slot claiming an oversized length is skipped and counted, never truncated.
//!
//! A device can be *detached*: it then moves nothing and the stack above it
//! keeps its state (addresses, timers), so losing and regaining the NIC driver
//! does not lose the lease.

use alloc::boxed::Box;

use framering::{Consumer, FrameBuf, PopError, Producer, PushError, MAX_FRAME};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;

/// Counters kept at the ring boundary; they survive re-attaching.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceStats {
    pub rx_frames: u64,
    pub tx_frames: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// Frames the stack built but the transmit ring had no room for.
    pub tx_dropped: u64,
    /// Receive slots whose length field was impossible.
    pub rx_bad_length: u64,
}

struct Rings {
    rx: Consumer,
    tx: Producer,
}

pub struct RingDevice {
    rings: Option<Rings>,
    /// Private copy of the frame being handed to smoltcp.
    rx_frame: Box<FrameBuf>,
    /// Staging area smoltcp builds an outgoing frame in.
    tx_frame: Box<FrameBuf>,
    max_frame: usize,
    stats: DeviceStats,
    /// Set when a frame was pushed to the transmit ring since the last
    /// [`RingDevice::take_tx_notify`] call.
    tx_pending: bool,
    poisoned: bool,
}

impl RingDevice {
    /// A device with no rings yet; `max_frame` is the driver's largest frame
    /// (MTU + 14).
    pub fn detached(max_frame: usize) -> RingDevice {
        RingDevice {
            rings: None,
            rx_frame: Box::new([0; MAX_FRAME]),
            tx_frame: Box::new([0; MAX_FRAME]),
            max_frame: max_frame.clamp(64, MAX_FRAME),
            stats: DeviceStats::default(),
            tx_pending: false,
            poisoned: false,
        }
    }

    /// A device attached to `rx` (consumed) and `tx` (produced).
    pub fn new(rx: Consumer, tx: Producer, max_frame: usize) -> RingDevice {
        let mut device = RingDevice::detached(max_frame);
        device.attach(rx, tx);
        device
    }

    /// Start using a fresh ring pair (and clear a poisoned state).
    pub fn attach(&mut self, rx: Consumer, tx: Producer) {
        self.rings = Some(Rings { rx, tx });
        self.poisoned = false;
        self.tx_pending = false;
    }

    /// Stop using the rings. The caller releases the shared memory.
    pub fn detach(&mut self) {
        self.rings = None;
        self.poisoned = false;
        self.tx_pending = false;
    }

    pub fn is_attached(&self) -> bool {
        self.rings.is_some()
    }

    pub fn stats(&self) -> &DeviceStats {
        &self.stats
    }

    /// Whether a ring peer broke the protocol. Nothing moves once this is set;
    /// the owner should [`RingDevice::detach`] and re-attach.
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// Whether anything is waiting in the receive ring.
    pub fn rx_pending(&mut self) -> bool {
        let Some(rings) = self.rings.as_mut() else {
            return false;
        };
        match rings.rx.pending() {
            Ok(n) => n > 0,
            Err(_) => {
                self.poisoned = true;
                false
            }
        }
    }

    /// Ask the driver to wake us when frames arrive (`Consumer::arm`).
    pub fn arm_rx(&mut self) {
        if let Some(rings) = self.rings.as_mut() {
            rings.rx.arm();
        }
    }

    /// After a burst of transmits: whether the driver armed its ring and
    /// wants a `Kick`. Clears the pending flag.
    pub fn take_tx_notify(&mut self) -> bool {
        if !core::mem::take(&mut self.tx_pending) {
            return false;
        }
        self.rings
            .as_mut()
            .is_some_and(|rings| rings.tx.take_notify())
    }

    /// Whether the transmit ring has room for at least one frame (a poisoned
    /// or detached device never does).
    fn tx_ready(&mut self) -> bool {
        if self.poisoned {
            return false;
        }
        let Some(rings) = self.rings.as_mut() else {
            return false;
        };
        match rings.tx.free() {
            Ok(free) => free > 0,
            Err(_) => {
                self.poisoned = true;
                false
            }
        }
    }

    /// Pop the next well-formed frame into the private buffer; its length.
    fn pop(&mut self) -> Option<usize> {
        if self.poisoned {
            return None;
        }
        let rings = self.rings.as_mut()?;
        loop {
            match rings.rx.pop(&mut self.rx_frame) {
                Ok(Some(n)) => {
                    self.stats.rx_frames += 1;
                    self.stats.rx_bytes += n as u64;
                    return Some(n);
                }
                Ok(None) => return None,
                Err(PopError::BadLength(_)) => self.stats.rx_bad_length += 1,
                Err(PopError::Corrupt) => {
                    self.poisoned = true;
                    return None;
                }
            }
        }
    }
}

pub struct RingRx<'a> {
    frame: &'a [u8],
}

pub struct RingTx<'a> {
    tx: &'a mut Producer,
    buf: &'a mut FrameBuf,
    stats: &'a mut DeviceStats,
    pending: &'a mut bool,
    poisoned: &'a mut bool,
}

impl RxToken for RingRx<'_> {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(self.frame)
    }
}

impl TxToken for RingTx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        // smoltcp never asks for more than the capabilities' MTU; if it did,
        // the excess is simply not sent.
        let len = len.min(self.buf.len());
        let result = f(&mut self.buf[..len]);
        match self.tx.push(&self.buf[..len]) {
            Ok(()) => {
                self.stats.tx_frames += 1;
                self.stats.tx_bytes += len as u64;
                *self.pending = true;
            }
            Err(PushError::Corrupt) => {
                *self.poisoned = true;
                self.stats.tx_dropped += 1;
            }
            Err(_) => self.stats.tx_dropped += 1,
        }
        result
    }
}

impl Device for RingDevice {
    type RxToken<'a> = RingRx<'a>;
    type TxToken<'a> = RingTx<'a>;

    fn receive(&mut self, _timestamp: Instant) -> Option<(RingRx<'_>, RingTx<'_>)> {
        // A reply needs room in the transmit ring: without it the frame stays
        // in the receive ring until there is, rather than being processed and
        // its answer lost silently.
        if !self.tx_ready() {
            return None;
        }
        let n = self.pop()?;
        let RingDevice {
            rings,
            rx_frame,
            tx_frame,
            stats,
            tx_pending,
            poisoned,
            ..
        } = self;
        let rings = rings.as_mut()?;
        Some((
            RingRx {
                frame: &rx_frame[..n],
            },
            RingTx {
                tx: &mut rings.tx,
                buf: tx_frame,
                stats,
                pending: tx_pending,
                poisoned,
            },
        ))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<RingTx<'_>> {
        if !self.tx_ready() {
            return None;
        }
        let RingDevice {
            rings,
            tx_frame,
            stats,
            tx_pending,
            poisoned,
            ..
        } = self;
        let rings = rings.as_mut()?;
        Some(RingTx {
            tx: &mut rings.tx,
            buf: tx_frame,
            stats,
            pending: tx_pending,
            poisoned,
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = self.max_frame;
        caps.max_burst_size = None;
        caps
    }
}
