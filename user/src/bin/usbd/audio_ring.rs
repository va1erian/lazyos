//! The isochronous side of a USB audio stream (docs/usb-audio-plan.md,
//! "Pacing" and "Completion"; xHCI 4.11.2.5, 4.14.2).
//!
//! One TD per service interval, one Isoch TRB each with Start Isoch ASAP.
//! Each TD's samples sit in a power-of-two slot of the controller's audio
//! region (at most 4 KiB, so no TRB crosses a page, hence no 64 KiB
//! boundary). [`IsoStream::fill`] keeps [`LEAD_US`] of audio queued: the
//! periods `sndd` submitted, converted to the device's layout, and silence
//! when there are none but only enough to keep [`LOW_WATER_US`] queued, so
//! real samples are never stuck behind much silence. A TD carrying the last
//! frame of a period, one TD every [`IOC_US`] and the last TD of every fill
//! ask for an event; TDs complete in ring order, so an event retires every
//! TD up to it.
//!
//! An asynchronous device's feedback endpoint keeps [`FEEDBACK_DEPTH`]
//! small IN transfers queued; each report goes to the pacer.

use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;
use user::trace;

use usbaudio::convert::{convert, SOURCE_FRAME};
use usbaudio::desc::Format;
use usbaudio::packet::Pacer;
use user::sys;
use xhci::ring::{ProducerRing, RawMem};
use xhci::trb::{self, code, Trb};

use super::mem::Region;
use super::Error;

/// Layout of the controller's audio region: the data ring, the feedback
/// ring and its report buffers, then the sample slots.
pub(super) const RING_TRBS: usize = 256;
const FEEDBACK_RING: usize = RING_TRBS * 16;
const FEEDBACK_TRBS: usize = 16;
const FEEDBACK_BUFFERS: usize = FEEDBACK_RING + FEEDBACK_TRBS * 16;
const FEEDBACK_BYTES: usize = 64;
const FEEDBACK_DEPTH: usize = 4;
const SLOTS: usize = 2 * 4096;
const SLOT_AREA: usize = 128 * 1024;
pub(super) const AUDIO_BYTES: usize = SLOTS + SLOT_AREA;
const _: () = assert!(FEEDBACK_BUFFERS + FEEDBACK_DEPTH * FEEDBACK_BYTES <= SLOTS);
/// The largest TD buffer: one page.
pub(super) const MAX_SLOT: usize = 4096;
/// Audio kept queued ahead of the bus, and the least silence keeps queued.
const LEAD_US: u32 = 32_000;
const LOW_WATER_US: u32 = 16_000;
/// One TD in this much audio asks for an event, whatever else happens.
const IOC_US: u32 = 2_000;

/// The staging buffer `sndd` shared: periods are copied out of it, never
/// referenced, so a rewrite changes what plays, never what `usbd` touches.
pub(super) struct Staging {
    pub(super) handle: u64,
    base: *const u8,
    len: usize,
}

impl Staging {
    /// Map the shared buffer `handle`, which must hold `len` bytes.
    pub(super) fn map(handle: u64, offset: u64, len: u64) -> Option<Staging> {
        let (va, size) = sys::buffer_map(handle).ok()?;
        let end = offset.checked_add(len)?;
        if end > size || len == 0 {
            return None;
        }
        Some(Staging {
            handle,
            base: (va as usize).checked_add(usize::try_from(offset).ok()?)? as *const u8,
            len: usize::try_from(len).ok()?,
        })
    }

    pub(super) fn len(&self) -> usize {
        self.len
    }

    /// Copy `out.len()` bytes at `offset`; the caller checked the range.
    fn read(&self, offset: usize, out: &mut [u8]) {
        assert!(offset + out.len() <= self.len);
        for (index, byte) in out.iter_mut().enumerate() {
            // SAFETY: inside the mapping (asserted), which lives until the
            // handle is closed. Volatile: `sndd` may write the same bytes at
            // any time, which yields wrong samples, never a wrong access.
            *byte = unsafe { self.base.add(offset + index).read_volatile() };
        }
    }
}

/// One submitted period: a range of the staging buffer and how much of it
/// is already in TDs.
struct Period {
    offset: usize,
    len: usize,
    taken: usize,
}

/// The periods `sndd` submitted and how many completed.
#[derive(Default)]
pub(super) struct Periods {
    queue: VecDeque<Period>,
    pub(super) submitted: u64,
    pub(super) completed: u64,
}

impl Periods {
    pub(super) fn push(&mut self, offset: usize, len: usize) {
        self.queue.push_back(Period {
            offset,
            len,
            taken: 0,
        });
        self.submitted += 1;
    }

    fn has_data(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Fill `out` from the queued periods; returns the bytes taken and how
    /// many periods ended in them.
    fn take(&mut self, staging: Option<&Staging>, out: &mut [u8]) -> (usize, u32) {
        let (mut got, mut ended) = (0, 0);
        while got < out.len() {
            let Some(period) = self.queue.front_mut() else {
                break;
            };
            let n = (period.len - period.taken).min(out.len() - got);
            match staging {
                Some(staging) => staging.read(period.offset + period.taken, &mut out[got..got + n]),
                None => out[got..got + n].fill(0),
            }
            period.taken += n;
            got += n;
            if period.taken == period.len {
                self.queue.pop_front();
                ended += 1;
            }
        }
        (got, ended)
    }

    /// Everything queued counts as played (a stop, a reconfiguration).
    pub(super) fn flush(&mut self) {
        self.queue.clear();
        self.completed = self.submitted;
    }
}

/// One TD in flight: its TRB and the periods that end in it.
struct Td {
    pointer: u64,
    ends: u32,
}

/// What a stream is configured with.
pub(super) struct Shape {
    pub(super) format: Format,
    pub(super) pacer: Pacer,
    pub(super) max_packet: u16,
    pub(super) transactions: u8,
    /// Service interval in microseconds.
    pub(super) esit_us: u32,
    pub(super) feedback_packet: Option<u16>,
}

/// The data (and feedback) rings of one configured alternate.
pub(super) struct IsoStream {
    shape: Shape,
    ring: ProducerRing<RawMem>,
    in_flight: VecDeque<Td>,
    slot_bytes: usize,
    slots: usize,
    next_slot: usize,
    lead: usize,
    low_water: usize,
    ioc_every: u64,
    tds: u64,
    feedback: Option<ProducerRing<RawMem>>,
    feedback_in_flight: VecDeque<(u64, usize)>,
    feedback_next: usize,
    source: Vec<u8>,
    out: Vec<u8>,
    pub(super) underruns: u32,
    pub(super) missed: u32,
    pub(super) errors: u32,
}

impl IsoStream {
    /// Rings over `region` (cleared here) for a stream of `shape`.
    pub(super) fn new(region: &mut Region, shape: Shape) -> Result<IsoStream, Error> {
        region.clear(0, SLOTS);
        let frame = shape.format.frame_bytes() as usize;
        let slot_frames = shape.pacer.slot_frames() as usize;
        let slot_bytes = (slot_frames * frame).next_power_of_two().max(64);
        if slot_bytes > MAX_SLOT || slot_frames == 0 {
            return Err(Error::Descriptor("audio packet larger than a page"));
        }
        let slots = SLOT_AREA / slot_bytes;
        let per = |us: u32| (us / shape.esit_us.max(1)).max(1) as usize;
        let lead = per(LEAD_US).min(slots - 1).min(RING_TRBS - 3);
        let ring = ProducerRing::new(region.ring(0, RING_TRBS)).map_err(Error::Xhci)?;
        let feedback = match shape.feedback_packet {
            Some(_) => Some(
                ProducerRing::new(region.ring(FEEDBACK_RING, FEEDBACK_TRBS))
                    .map_err(Error::Xhci)?,
            ),
            None => None,
        };
        Ok(IsoStream {
            low_water: per(LOW_WATER_US).min(lead),
            ioc_every: per(IOC_US) as u64,
            source: vec![0; slot_frames * SOURCE_FRAME],
            out: vec![0; slot_bytes],
            shape,
            ring,
            in_flight: VecDeque::new(),
            slot_bytes,
            slots,
            next_slot: 0,
            lead,
            tds: 0,
            feedback,
            feedback_in_flight: VecDeque::new(),
            feedback_next: 0,
            underruns: 0,
            missed: 0,
            errors: 0,
        })
    }

    /// The data ring's dequeue pointer, for the endpoint context.
    pub(super) fn dequeue(&self) -> u64 {
        self.ring.dequeue_pointer()
    }

    pub(super) fn feedback_dequeue(&self) -> Option<u64> {
        self.feedback.as_ref().map(ProducerRing::dequeue_pointer)
    }

    /// TDs kept queued ahead of the bus.
    pub(super) fn lead(&self) -> usize {
        self.lead
    }

    /// Queue TDs up to the lead; whether any was added (ring the doorbell).
    pub(super) fn fill(
        &mut self,
        region: &mut Region,
        periods: &mut Periods,
        staging: Option<&Staging>,
    ) -> Result<bool, Error> {
        let mut added = false;
        while self.in_flight.len() < self.lead && self.ring.free() > 0 {
            if !periods.has_data() && self.in_flight.len() >= self.low_water {
                break;
            }
            let frames = self.shape.pacer.next_frames() as usize;
            let frame = self.shape.format.frame_bytes() as usize;
            let bytes = (frames * frame).min(self.slot_bytes);
            let wanted = (bytes / frame) * SOURCE_FRAME;
            let had_data = periods.has_data();
            let (got, ends) = periods.take(staging, &mut self.source[..wanted]);
            if had_data && got < wanted {
                // `sndd` was late: the rest of this interval is silence.
                self.underruns += 1;
            }
            let written = convert(&self.source[..got], &self.shape.format, &mut self.out);
            self.out[written..bytes].fill(0);
            let offset = SLOTS + self.next_slot * self.slot_bytes;
            region.write(offset, &self.out[..bytes]);
            self.next_slot = (self.next_slot + 1) % self.slots;
            let last = self.in_flight.len() + 1 >= self.lead
                || (!periods.has_data() && self.in_flight.len() + 1 >= self.low_water);
            let ioc = ends > 0 || last || self.tds.is_multiple_of(self.ioc_every);
            let max_packet = usize::from(self.shape.max_packet.max(1));
            let packets = bytes
                .div_ceil(max_packet)
                .clamp(1, usize::from(self.shape.transactions)) as u8;
            let trb = trb::isoch(region.bus(offset), bytes as u32, packets, ioc, false)
                .ok_or(Error::Descriptor("isoch TD"))?;
            let pointer = self.ring.enqueue(&[trb], false).map_err(Error::Xhci)?;
            self.in_flight.push_back(Td { pointer, ends });
            self.tds += 1;
            added = true;
        }
        Ok(added)
    }

    /// A transfer event on the data endpoint: retire the TDs it covers and
    /// count their periods as completed.
    pub(super) fn complete(&mut self, event: &Trb, periods: &mut Periods) {
        let status = event.completion_code();
        if status == code::RING_UNDERRUN || status == code::RING_OVERRUN {
            // Every TD was consumed (events of those without IOC never
            // come): the ring is empty and the next TD restarts it (SIA).
            self.underruns += 1;
            periods.completed += self
                .in_flight
                .drain(..)
                .map(|td| u64::from(td.ends))
                .sum::<u64>();
            let _ = self.ring.abandon();
            return;
        }
        let Some(position) = self
            .in_flight
            .iter()
            .position(|td| td.pointer == event.parameter)
        else {
            return;
        };
        match status {
            code::SUCCESS | code::SHORT_PACKET => {}
            code::MISSED_SERVICE => self.missed += 1,
            other => {
                self.errors += 1;
                static XFER: tracefmt::Budget = tracefmt::Budget::new(20, 1000);
                if trace::within(&XFER, trace::Channel::Usbd) {
                    trace::line!(
                        trace::Channel::Usbd,
                        "AUDIO:XFER code={other} tds={}",
                        self.tds
                    );
                }
            }
        }
        let _ = self.ring.retire(event.parameter);
        periods.completed += self
            .in_flight
            .drain(..=position)
            .map(|td| u64::from(td.ends))
            .sum::<u64>();
    }

    /// Keep the feedback endpoint's reads queued; whether any was added.
    pub(super) fn fill_feedback(&mut self, region: &Region) -> Result<bool, Error> {
        let (Some(ring), Some(packet)) = (self.feedback.as_mut(), self.shape.feedback_packet)
        else {
            return Ok(false);
        };
        let len = u32::from(packet.clamp(1, FEEDBACK_BYTES as u16));
        let mut added = false;
        while self.feedback_in_flight.len() < FEEDBACK_DEPTH {
            let buffer = self.feedback_next;
            self.feedback_next = (buffer + 1) % FEEDBACK_DEPTH;
            let bus = region.bus(FEEDBACK_BUFFERS + buffer * FEEDBACK_BYTES);
            let trb =
                trb::isoch(bus, len, 1, true, true).ok_or(Error::Descriptor("feedback TD"))?;
            let pointer = ring.enqueue(&[trb], false).map_err(Error::Xhci)?;
            self.feedback_in_flight.push_back((pointer, buffer));
            added = true;
        }
        Ok(added)
    }

    /// A transfer event on the feedback endpoint: hand the report to the pacer.
    pub(super) fn feedback_report(&mut self, event: &Trb, region: &Region) {
        let Some(ring) = self.feedback.as_mut() else {
            return;
        };
        let Some(position) = self
            .feedback_in_flight
            .iter()
            .position(|&(pointer, _)| pointer == event.parameter)
        else {
            if event.completion_code() == code::RING_OVERRUN {
                self.feedback_in_flight.clear();
                let _ = ring.abandon();
            }
            return;
        };
        let _ = ring.retire(event.parameter);
        let (_, buffer) = self
            .feedback_in_flight
            .drain(..=position)
            .next_back()
            .unwrap_or((0, 0));
        if matches!(event.completion_code(), code::SUCCESS | code::SHORT_PACKET) {
            let packet = usize::from(self.shape.feedback_packet.unwrap_or(4)).min(FEEDBACK_BYTES);
            let len = packet.saturating_sub(event.residual() as usize);
            let mut report = [0u8; FEEDBACK_BYTES];
            region.read(
                FEEDBACK_BUFFERS + buffer * FEEDBACK_BYTES,
                &mut report[..len],
            );
            self.shape.pacer.feedback(&report[..len.min(4)]);
        }
    }

    /// The pace the device asked for, in Hz.
    pub(super) fn rate_hz(&self) -> u32 {
        self.shape.pacer.rate_hz()
    }

    /// Forget everything in flight (the endpoints were stopped); the
    /// pointers for Set TR Dequeue Pointer.
    pub(super) fn abandon(&mut self) -> (u64, Option<u64>) {
        self.in_flight.clear();
        self.feedback_in_flight.clear();
        (
            self.ring.abandon(),
            self.feedback.as_mut().map(ProducerRing::abandon),
        )
    }
}
