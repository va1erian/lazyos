//! The virtio-sound card: its four virtqueues, the control-request path and the
//! transmit path.
//!
//! A request is queued, the device kicked, and the used ring polled. Between
//! looks the driver waits on the device's interrupt endpoint for at most one
//! tick (`wait_event`): an armed INTx line wakes it at once, and on a line that
//! is not routable it degrades to plain one-tick polling, the CI-safe default of
//! `docs/driver-plan.md` section 3.3. Audio periods are tens of milliseconds
//! long, far coarser than the 10 ms tick either way.

use alloc::vec::Vec;

use user::sys;
use virtio::queue::{Buf, Virtqueue};
use virtio::transport::Kick;
use virtio_snd::queue as qi;
use virtio_snd::wire::{self, code, status, PcmInfo};

use super::device::{self, Claimed};
use super::dma::Region;
use super::error::Error;

/// Streams the driver will describe; more are ignored.
pub(super) const MAX_STREAMS: usize = 8;
/// Transmit slots (periods in flight) a stream may use.
pub(super) const MAX_SLOTS: usize = 8;

/// Bytes of DMA staging shared by every stream, allocated once. A 48 kHz
/// stereo stream of four 8 KiB periods needs 32 KiB; this leaves headroom for
/// bigger periods.
const STAGING_BYTES: usize = 64 * 1024;

/// Ticks (100 Hz) to wait for one device answer before giving up.
const TIMEOUT_TICKS: u64 = 500;

// Layout of the shared control block (one DMA region).
const CONTROL_Q: usize = 0;
const EVENT_Q: usize = 512;
const RX_Q: usize = 1024;
const TX_Q: usize = 1536;
const CTRL_REQ: usize = 4096;
const CTRL_REQ_LEN: usize = 256;
const CTRL_RESP: usize = 4352;
const CTRL_RESP_LEN: usize = 2048;
const TX_META: usize = 8192;
const TX_META_STRIDE: usize = 16;
const CORE_BYTES: usize = 12288;

const CONTROL_SIZE: u16 = 8;
const EVENT_SIZE: u16 = 8;
const RX_SIZE: u16 = 8;
const TX_SIZE: u16 = 32;

// The queues must fit their windows and the windows must not overlap.
const _: () = {
    assert!(Virtqueue::bytes_needed(CONTROL_SIZE) <= EVENT_Q - CONTROL_Q);
    assert!(Virtqueue::bytes_needed(EVENT_SIZE) <= RX_Q - EVENT_Q);
    assert!(Virtqueue::bytes_needed(RX_SIZE) <= TX_Q - RX_Q);
    assert!(Virtqueue::bytes_needed(TX_SIZE) <= CTRL_REQ - TX_Q);
    assert!(CTRL_REQ + CTRL_REQ_LEN <= CTRL_RESP);
    assert!(CTRL_RESP + CTRL_RESP_LEN <= TX_META);
    assert!(TX_META + MAX_SLOTS * TX_META_STRIDE <= CORE_BYTES);
    assert!(MAX_SLOTS <= TX_SIZE as usize / 3);
};

struct Queue {
    vq: Virtqueue,
    kick: Kick,
}

pub(super) struct Card {
    claimed: Claimed,
    core: Region,
    control: Queue,
    // Set up so the device sees all four queues; the driver uses only
    // control and transmit today.
    _event: Queue,
    _rx: Queue,
    tx: Queue,
    /// DMA staging a stream borrows; `None` while one is open. Never freed:
    /// freeing a DMA buffer stops the device (see `dma::Region`).
    staging: Option<Region>,
    /// Slot each in-flight transmit head belongs to (`u8::MAX` when idle).
    slot_of_head: [u8; virtio::queue::MAX_QUEUE],
    /// Number of streams the device reports.
    pub(super) streams: u32,
    /// Control requests that timed out and that the device has not returned
    /// yet. The request and reply buffers are shared by every control request,
    /// so while one is outstanding the device may still read or write them:
    /// no new request is built until each has been handed back.
    stale_control: u32,
    /// Interrupt messages serviced (0 when polling alone).
    irqs: u64,
    /// Receive buffer for interrupt messages, allocated once.
    irq_buf: alloc::vec::Vec<u8>,
}

/// Sleep one PIT tick ([`sys::nap`]).
fn nap() {
    sys::nap();
}

impl Card {
    /// Claim the device, negotiate features, set up the queues and go live.
    pub(super) fn open() -> Result<Card, Error> {
        let claimed = device::open()?;
        claimed.transport.negotiate(0, 0)?;
        let core = Region::alloc(claimed.handle, CORE_BYTES)?;

        let make = |offset: usize, size: u16, index: u16| -> Result<Queue, Error> {
            // SAFETY: the window `offset..offset+bytes_needed(size)` is inside
            // the region (checked by the const asserts above), 4-aligned, and
            // only this queue touches it while the device works on it.
            let vq = unsafe { Virtqueue::new(core.ptr(offset)?, core.bus(offset), size) }?;
            let kick = claimed.transport.setup_queue(index, &vq)?;
            Ok(Queue { vq, kick })
        };
        let control = make(CONTROL_Q, CONTROL_SIZE, qi::CONTROL)?;
        let event = make(EVENT_Q, EVENT_SIZE, qi::EVENT)?;
        let rx = make(RX_Q, RX_SIZE, qi::RX)?;
        let tx = make(TX_Q, TX_SIZE, qi::TX)?;
        claimed.transport.driver_ok()?;
        let staging = Region::alloc(claimed.handle, STAGING_BYTES)?;

        let streams = claimed
            .transport
            .device_config(wire::config::STREAMS, 4)
            .map_err(Error::Virtio)?;
        Ok(Card {
            claimed,
            core,
            control,
            _event: event,
            _rx: rx,
            tx,
            staging: Some(staging),
            slot_of_head: [u8::MAX; virtio::queue::MAX_QUEUE],
            streams,
            stale_control: 0,
            irqs: 0,
            irq_buf: alloc::vec![0u8; 256],
        })
    }

    /// Borrow the DMA staging for a stream; [`Error::Busy`] if one is open.
    pub(super) fn take_staging(&mut self) -> Result<Region, Error> {
        self.staging.take().ok_or(Error::Busy)
    }

    /// Return the staging a finished stream borrowed.
    pub(super) fn give_back(&mut self, region: Region) {
        self.staging = Some(region);
    }

    /// Run one control request: `request` out, up to `response_len` bytes back.
    /// Returns the reply bytes the device wrote (status word included); an
    /// error status becomes [`Error::Status`].
    fn control(&mut self, request: &[u8], response_len: usize) -> Result<Vec<u8>, Error> {
        if request.len() > CTRL_REQ_LEN || response_len > CTRL_RESP_LEN {
            return Err(Error::Range);
        }
        self.reclaim_stale_control()?;
        self.core
            .bytes(CTRL_REQ, request.len())?
            .copy_from_slice(request);
        self.core.bytes(CTRL_RESP, response_len)?.fill(0);
        let head = self.control.vq.add(&[
            Buf {
                bus: self.core.bus(CTRL_REQ),
                len: request.len() as u32,
                device_writes: false,
            },
            Buf {
                bus: self.core.bus(CTRL_RESP),
                len: response_len as u32,
                device_writes: true,
            },
        ])?;
        self.claimed.transport.notify(self.control.kick);
        let deadline = sys::clock() + TIMEOUT_TICKS;
        loop {
            match self.control.vq.pop_used()? {
                Some(used) if used.head == head => break,
                // Exactly one request is outstanding here (earlier timeouts
                // were reclaimed before this one was built), so any other head
                // is a device fault.
                Some(_) => return Err(Error::Virtio(virtio::Error::DeviceError)),
                None if sys::clock() >= deadline => {
                    self.stale_control = self.stale_control.saturating_add(1);
                    let code = wire::parse_status(request).unwrap_or(0);
                    sys::write_str(&alloc::format!(
                        "SNDD:CTL:TIMEOUT request={code:#x}
"
                    ));
                    return Err(Error::Timeout);
                }
                None => self.wait_event(),
            }
        }
        // Read the whole reply buffer, not the length the device put in the
        // used ring: that length is untrusted, and devices differ in what they
        // report (QEMU 8.2 counts only the status word for `PCM_INFO` even
        // though it wrote every record). The buffer was zeroed before the
        // request, so a device that wrote less leaves zeros, which parse as a
        // stream with no formats and fail closed in `params::grant`.
        let reply = self.core.bytes(CTRL_RESP, response_len)?.to_vec();
        match wire::parse_status(&reply) {
            Some(status::OK) => Ok(reply),
            Some(other) => Err(Error::Status(other)),
            None => Err(Error::Status(0)),
        }
    }

    /// Take back the completions of control requests that timed out. Until the
    /// device has returned every one, the shared request/reply buffers may
    /// still be in use by it, so a new request is refused (`Timeout`, the same
    /// answer the caller already saw) rather than overwriting them.
    fn reclaim_stale_control(&mut self) -> Result<(), Error> {
        while self.stale_control > 0 {
            match self.control.vq.pop_used()? {
                Some(_) => self.stale_control -= 1,
                None => return Err(Error::Timeout),
            }
        }
        Ok(())
    }

    /// Describe every stream the device reports (up to [`MAX_STREAMS`]).
    pub(super) fn pcm_infos(&mut self) -> Result<Vec<PcmInfo>, Error> {
        let count = (self.streams as usize).min(MAX_STREAMS);
        if count == 0 {
            return Ok(Vec::new());
        }
        let request = wire::pcm_info_request(0, count as u32);
        let reply = self.control(&request, wire::STATUS_LEN + count * wire::PCM_INFO_SIZE)?;
        let records = reply.get(wire::STATUS_LEN..).ok_or(Error::Range)?;
        (0..count)
            .map(|index| PcmInfo::parse(records, index).ok_or(Error::Range))
            .collect()
    }

    pub(super) fn set_params(
        &mut self,
        stream: u32,
        buffer_bytes: u32,
        period_bytes: u32,
        channels: u8,
        format: u8,
        rate: u8,
    ) -> Result<(), Error> {
        let request =
            wire::set_params_request(stream, buffer_bytes, period_bytes, channels, format, rate);
        self.control(&request, wire::STATUS_LEN).map(|_| ())
    }

    /// `PREPARE`, `START`, `STOP` or `RELEASE` on `stream`.
    pub(super) fn stream_op(&mut self, op: StreamOp, stream: u32) -> Result<(), Error> {
        let code = match op {
            StreamOp::Prepare => code::PCM_PREPARE,
            StreamOp::Start => code::PCM_START,
            StreamOp::Stop => code::PCM_STOP,
            StreamOp::Release => code::PCM_RELEASE,
        };
        self.control(&wire::stream_request(code, stream), wire::STATUS_LEN)
            .map(|_| ())
    }

    /// Queue transmit slot `slot` of `ring` (`len` bytes at `slot * period`).
    pub(super) fn submit(
        &mut self,
        stream: u32,
        ring: &Region,
        slot: usize,
        period_bytes: usize,
        len: usize,
    ) -> Result<(), Error> {
        if slot >= MAX_SLOTS || len > period_bytes {
            return Err(Error::Range);
        }
        let ring_offset = slot.checked_mul(period_bytes).ok_or(Error::Range)?;
        if ring_offset
            .checked_add(len)
            .is_none_or(|end| end > ring.len())
        {
            return Err(Error::Range);
        }
        let meta = TX_META + slot * TX_META_STRIDE;
        self.core
            .bytes(meta, wire::XFER_HEADER_LEN)?
            .copy_from_slice(&wire::xfer_header(stream));
        self.core.bytes(meta + 8, wire::XFER_STATUS_LEN)?.fill(0);
        let head = self.tx.vq.add(&[
            Buf {
                bus: self.core.bus(meta),
                len: wire::XFER_HEADER_LEN as u32,
                device_writes: false,
            },
            Buf {
                bus: ring.bus(ring_offset),
                len: len as u32,
                device_writes: false,
            },
            Buf {
                bus: self.core.bus(meta + 8),
                len: wire::XFER_STATUS_LEN as u32,
                device_writes: true,
            },
        ])?;
        self.slot_of_head[usize::from(head)] = slot as u8;
        self.claimed.transport.notify(self.tx.kick);
        Ok(())
    }

    /// Reap one finished transmit, if any: the slot it used and whether the
    /// device reported success.
    pub(super) fn reap(&mut self) -> Result<Option<(usize, bool)>, Error> {
        let Some(used) = self.tx.vq.pop_used()? else {
            return Ok(None);
        };
        let slot = core::mem::replace(&mut self.slot_of_head[usize::from(used.head)], u8::MAX);
        if usize::from(slot) >= MAX_SLOTS {
            return Err(Error::Virtio(virtio::Error::DeviceError));
        }
        let meta = TX_META + usize::from(slot) * TX_META_STRIDE + 8;
        let reply = self.core.bytes(meta, wire::XFER_STATUS_LEN)?.to_vec();
        let ok = matches!(wire::parse_xfer_status(&reply), Some((status::OK, _)));
        Ok(Some((usize::from(slot), ok)))
    }

    /// Wait briefly for the device: an interrupt if the line is armed, at most
    /// one tick either way, so a lost or unrouted interrupt only costs the
    /// polling latency the driver would have had anyway.
    pub(super) fn wait_event(&mut self) {
        let Some(irq) = self.claimed.irq else {
            return nap();
        };
        // A timeout or transient error falls through: the caller polls the
        // used ring.
        if let Ok(message) = irq.recv_with(&mut self.irq_buf, Some(sys::clock() + 1)) {
            self.handle_irq(&message);
        }
    }

    /// Service any interrupt message already queued, without waiting for one.
    pub(super) fn service_irq(&mut self) {
        let Some(irq) = self.claimed.irq else {
            return;
        };
        if let Ok(Some(message)) = irq.poll_recv_with(&mut self.irq_buf) {
            self.handle_irq(&message);
        }
    }

    /// Acknowledge one interrupt: only the kernel (slot 0) may send one, and it
    /// must name this device. Reading the ISR status deasserts the level
    /// interrupt before the kernel is told to unmask the line.
    fn handle_irq(&mut self, message: &user::messenger::Message) {
        let genuine =
            message.sender == 0 && user::dev::parse_irq_body(&message.parcel.body).is_some();
        if !genuine {
            sys::write_str(&alloc::format!(
                "SNDD:IRQ:REJECT sender={} body={}
",
                message.sender,
                message.parcel.body.len()
            ));
            return;
        }
        self.irqs += 1;
        let _ = self.claimed.transport.isr_status();
        let _ = user::dev::irq_ack(self.claimed.handle);
    }

    /// Whether the interrupt line is armed, and how many interrupts arrived.
    pub(super) fn irq_report(&self) -> (bool, u64) {
        (self.claimed.irq.is_some(), self.irqs)
    }

    /// Sleep briefly while waiting on the device.
    pub(super) fn idle(&mut self) {
        self.wait_event();
    }
}

#[derive(Clone, Copy)]
pub(super) enum StreamOp {
    Prepare,
    Start,
    Stop,
    Release,
}
