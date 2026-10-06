//! The Intel High Definition Audio card (issue #497, driver-plan D7): one
//! controller, the first codec on its link, the best output path through that
//! codec, and the first output stream descriptor (`libs/hda`).
//!
//! The stream and session code above the card speak in periods the driver
//! *submits* and the card *completes* (the virtio model). An HDA stream
//! instead plays a cyclic buffer forever, so this card presents that model:
//! the buffer is the stream's staging, one BDL entry per period; DMA starts
//! once every period holds samples (or as soon as the driver waits for one);
//! a period counts as complete when the link position has passed it, and is
//! then zeroed so that, if nothing new is submitted in time, the controller
//! replays silence rather than old samples.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::vec;
use alloc::vec::Vec;
use core::ptr;

use hda::codec::{find_output, read_graph, Graph, Path};
use hda::cursor::Cursor;
use hda::program::Output;
use hda::regs::{intctl, Regs, INTCTL, MIN_BAR_BYTES, RIRBSTS};
use hda::stream::{write_bdl, OutStream, BDL_ENTRY};
use hda::{format as fmt, Controller, Mmio, RingMemory};
use user::dev::Row;
use user::sys;
use virtio_snd::params::RATES;
use virtio_snd::wire::{direction, format, PcmInfo};

use super::card::{StreamOp, MAX_SLOTS};
use super::device::{self, Claimed};
use super::dma::Region;
use super::error::Error;

/// The register BAR.
const BAR: usize = 0;
/// The stream tag the descriptor and the converter share (any of 1..=15).
const TAG: u8 = 1;
/// Core DMA block: the command rings, then the BDL on its own 128-byte line.
const BDL_OFFSET: usize = 4096;
const CORE_BYTES: usize = BDL_OFFSET + MAX_SLOTS * BDL_ENTRY;
/// Stream staging, as for virtio-sound.
const STAGING_BYTES: usize = 64 * 1024;
/// BDL entries must start on 128-byte boundaries, so periods are multiples.
const ALIGN: u32 = 128;
/// Ticks a stop waits for the last period to play out (a period is tens of
/// milliseconds; this is a generous bound).
const PLAY_OUT_TICKS: u32 = 50;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Prepared,
    /// Started by the client; DMA runs once there is something to play.
    Armed,
    Running,
}

#[derive(Clone, Copy)]
struct Params {
    format: u16,
    period: u32,
    periods: u32,
}

pub(super) struct HdaCard {
    claimed: Claimed,
    controller: Controller<Mmio>,
    graph: Box<Graph>,
    path: Path,
    stream: OutStream,
    /// Held for the driver's lifetime: command rings and the BDL.
    core: Region,
    staging: Option<Region>,
    staging_va: *mut u8,
    staging_bus: u64,
    pcm: u32,
    channels: u8,
    params: Option<Params>,
    state: State,
    queued: [bool; MAX_SLOTS],
    cursor: Option<Cursor>,
    /// Completed slots not yet handed to the stream.
    done: VecDeque<usize>,
    irqs: u64,
    irq_buf: Vec<u8>,
}

fn hda_error<E: core::fmt::Debug>(what: &'static str) -> impl Fn(E) -> Error {
    move |error| {
        sys::write_str(&format!("SNDD:HDA:ERR {what}: {error:?}\n"));
        Error::Hda(what)
    }
}

impl HdaCard {
    /// Claim the controller `row`, find an output path and go live.
    pub(super) fn open(row: Row) -> Result<HdaCard, Error> {
        // The line may be shared with other functions on a real board.
        let mut claimed = device::claim(row, true)?;
        let base = device::map(&claimed, BAR, MIN_BAR_BYTES)?;
        // SAFETY: `device::map` mapped the whole BAR, at least
        // `MIN_BAR_BYTES` long, for as long as the claim lives.
        let regs = unsafe { Mmio::new(base, claimed.row.bar_len[BAR]) }.ok_or(Error::Range)?;
        let core = Region::alloc(claimed.handle, CORE_BYTES)?;
        let rings = RingMemory {
            va: core.ptr(0)?,
            bus: core.bus(0),
        };
        // SAFETY: the ring memory is the first `RING_BYTES` of `core`, page
        // aligned, used by the controller alone for the driver's lifetime.
        let mut controller =
            unsafe { Controller::new(regs, rings, sys::nap) }.map_err(hda_error("controller"))?;
        let codec = controller.codec;
        let graph = Box::new(read_graph(&mut controller, codec).map_err(hda_error("codec"))?);
        let path = find_output(&graph).map_err(hda_error("output path"))?;
        let output = Output {
            codec,
            graph: &graph,
            path,
        };
        output.enable(&mut controller).map_err(hda_error("path"))?;
        let pcm = output.pcm(&mut controller).map_err(hda_error("pcm"))?;
        let channels = output.channels();
        let stream = OutStream {
            base: controller.output_stream(0).ok_or(Error::NoStream)?,
        };
        let stream_bit = u32::from(controller.caps.inputs);
        if !stream.reset(controller.regs_mut(), sys::nap) {
            return Err(Error::Timeout);
        }
        let staging = Region::alloc(claimed.handle, STAGING_BYTES)?;
        let (staging_va, staging_bus) = (staging.ptr(0)?, staging.bus(0));
        device::arm(&mut claimed)?;
        if claimed.irq.is_some() {
            controller
                .regs_mut()
                .write32(INTCTL, intctl::GIE | 1 << stream_bit);
        }
        sys::write_str(&format!(
            "SNDD:HDA codec={codec} afg={} pin={} dac={} hops={} pcm={pcm:#x} channels={channels}\n",
            graph.afg,
            path.pin(),
            path.dac(),
            path.len
        ));
        Ok(HdaCard {
            claimed,
            controller,
            graph,
            path,
            stream,
            core,
            staging: Some(staging),
            staging_va,
            staging_bus,
            pcm,
            channels,
            params: None,
            state: State::Idle,
            queued: [false; MAX_SLOTS],
            cursor: None,
            done: VecDeque::new(),
            irqs: 0,
            irq_buf: vec![0u8; 256],
        })
    }

    pub(super) fn take_staging(&mut self) -> Result<Region, Error> {
        self.staging.take().ok_or(Error::Busy)
    }

    pub(super) fn give_back(&mut self, region: Region) {
        self.staging = Some(region);
    }

    /// The one output stream, as the virtio-sound `PCM_INFO` would describe it.
    pub(super) fn pcm_infos(&mut self) -> Result<Vec<PcmInfo>, Error> {
        let mut formats = 0u64;
        if fmt::supports_size(self.pcm, 16) {
            formats |= 1 << format::S16;
        }
        if fmt::supports_size(self.pcm, 32) {
            formats |= 1 << format::S32;
        }
        let rates = RATES
            .iter()
            .filter(|&&(hz, _)| fmt::supports_rate(self.pcm, hz))
            .fold(0u64, |mask, &(_, code)| mask | 1 << code);
        Ok(vec![PcmInfo {
            formats,
            rates,
            direction: direction::OUTPUT,
            channels_min: 1,
            channels_max: self.channels,
        }])
    }

    /// Bind the converter to stream `tag` (0 unbinds it) at `format`.
    fn bind(&mut self, tag: u8, format: u16) -> Result<(), Error> {
        let output = Output {
            codec: self.controller.codec,
            graph: &self.graph,
            path: self.path,
        };
        output
            .bind(&mut self.controller, tag, format)
            .map_err(hda_error("bind"))
    }

    pub(super) fn set_params(
        &mut self,
        stream: u32,
        buffer_bytes: u32,
        period_bytes: u32,
        channels: u8,
        format_code: u8,
        rate_code: u8,
    ) -> Result<(), Error> {
        if stream != 0 {
            return Err(Error::Params);
        }
        let bits = match format_code {
            format::S16 => 16,
            format::S32 => 32,
            _ => return Err(Error::Unsupported),
        };
        let hz = RATES
            .iter()
            .find(|&&(_, code)| code == rate_code)
            .map(|&(hz, _)| hz)
            .ok_or(Error::Params)?;
        let word = fmt::encode(hz, bits, u32::from(channels)).ok_or(Error::Unsupported)?;
        if period_bytes == 0
            || !period_bytes.is_multiple_of(ALIGN)
            || !buffer_bytes.is_multiple_of(period_bytes)
        {
            return Err(Error::Unsupported);
        }
        let periods = buffer_bytes / period_bytes;
        if periods < 2 || periods as usize > MAX_SLOTS || buffer_bytes as usize > STAGING_BYTES {
            return Err(Error::Unsupported);
        }
        self.params = Some(Params {
            format: word,
            period: period_bytes,
            periods,
        });
        Ok(())
    }

    pub(super) fn stream_op(&mut self, op: StreamOp, stream: u32) -> Result<(), Error> {
        if stream != 0 {
            return Err(Error::Params);
        }
        match op {
            StreamOp::Prepare => self.prepare(),
            StreamOp::Start => {
                if self.state != State::Prepared {
                    return Err(Error::Params);
                }
                self.state = State::Armed;
                Ok(())
            }
            StreamOp::Stop => {
                self.play_out();
                self.stop_dma()?;
                // Nothing queued will play now: hand every slot back.
                for slot in 0..MAX_SLOTS {
                    if core::mem::take(&mut self.queued[slot]) {
                        self.done.push_back(slot);
                    }
                }
                self.state = State::Prepared;
                Ok(())
            }
            StreamOp::Release => {
                self.stop_dma()?;
                if !self.stream.reset(self.controller.regs_mut(), sys::nap) {
                    return Err(Error::Timeout);
                }
                let format = self.params.map_or(0, |params| params.format);
                self.bind(0, format)?;
                self.queued = [false; MAX_SLOTS];
                self.done.clear();
                self.state = State::Idle;
                Ok(())
            }
        }
    }

    /// Program the descriptor and the converter for the parameters, with a
    /// silent buffer.
    fn prepare(&mut self) -> Result<(), Error> {
        let params = self.params.ok_or(Error::Params)?;
        self.stop_dma()?;
        if !self.stream.reset(self.controller.regs_mut(), sys::nap) {
            return Err(Error::Timeout);
        }
        let buffer = params.period * params.periods;
        // SAFETY: the staging region is `STAGING_BYTES` long and `buffer` fits
        // it (`set_params`); the stream is stopped, so the controller is not
        // reading it, and the driver's own writes happen between calls.
        unsafe { ptr::write_bytes(self.staging_va, 0, buffer as usize) };
        // SAFETY: the BDL area lies inside `core` (`CORE_BYTES` covers
        // `MAX_SLOTS` entries) and the stream is stopped.
        unsafe {
            write_bdl(
                self.core.ptr(BDL_OFFSET)?,
                self.staging_bus,
                params.period,
                params.periods as usize,
            )
        };
        self.stream.program(
            self.controller.regs_mut(),
            TAG,
            self.core.bus(BDL_OFFSET),
            buffer,
            params.periods as u16,
            params.format,
        );
        self.bind(TAG, params.format)?;
        self.cursor = Cursor::new(params.period, params.periods);
        self.queued = [false; MAX_SLOTS];
        self.done.clear();
        self.state = State::Prepared;
        Ok(())
    }

    /// Let one more period play before a stop. The link position counts what
    /// the controller fetched, and a codec buffers about a period ahead of
    /// what it outputs, so stopping the moment the last period was fetched
    /// would cut its end off. What plays meanwhile is silence (every played
    /// period is zeroed). Bounded: a stalled stream stops anyway.
    fn play_out(&mut self) {
        if self.state != State::Running {
            return;
        }
        let Some(target) = self.cursor.map(|cursor| cursor.completed() + 1) else {
            return;
        };
        for _ in 0..PLAY_OUT_TICKS {
            self.advance();
            if self
                .cursor
                .is_some_and(|cursor| cursor.completed() >= target)
            {
                return;
            }
            sys::nap();
        }
    }

    fn start_dma(&mut self) -> Result<(), Error> {
        if !self.stream.run(self.controller.regs_mut(), true, sys::nap) {
            return Err(Error::Timeout);
        }
        self.state = State::Running;
        Ok(())
    }

    fn stop_dma(&mut self) -> Result<(), Error> {
        if self.state == State::Running {
            if !self.stream.run(self.controller.regs_mut(), false, sys::nap) {
                return Err(Error::Timeout);
            }
            self.state = State::Armed;
        }
        Ok(())
    }

    /// Silence slot `slot` (so a replay before the next submit is quiet).
    fn silence(&self, slot: usize, from: usize) {
        if let Some(params) = self.params {
            let period = params.period as usize;
            if slot < params.periods as usize && from < period {
                // SAFETY: the slot lies inside the staging buffer (`slot <
                // periods`, `periods * period <= STAGING_BYTES`).
                unsafe {
                    ptr::write_bytes(self.staging_va.add(slot * period + from), 0, period - from)
                };
            }
        }
    }

    pub(super) fn submit(
        &mut self,
        stream: u32,
        ring: &Region,
        slot: usize,
        period_bytes: usize,
        len: usize,
    ) -> Result<(), Error> {
        let params = self.params.ok_or(Error::Params)?;
        let periods = params.periods as usize;
        if stream != 0
            || slot >= periods
            || period_bytes != params.period as usize
            || len > period_bytes
            || ring.bus(0) != self.staging_bus
        {
            return Err(Error::Range);
        }
        // A short final period: the rest of the slot must be silence.
        self.silence(slot, len);
        self.queued[slot] = true;
        if self.state == State::Armed && self.queued[..periods].iter().all(|&queued| queued) {
            self.start_dma()?;
        }
        Ok(())
    }

    /// One completed slot, if any (always successful: the controller has no
    /// per-buffer status).
    pub(super) fn reap(&mut self) -> Result<Option<(usize, bool)>, Error> {
        if self.done.is_empty() {
            match self.state {
                // The driver is waiting for a slot: play what there is.
                State::Armed if self.queued.iter().any(|&queued| queued) => self.start_dma()?,
                State::Running => self.advance(),
                _ => {}
            }
        }
        Ok(self.done.pop_front().map(|slot| (slot, true)))
    }

    /// Count the periods the link position has passed.
    fn advance(&mut self) {
        let Some(params) = self.params else { return };
        let position = self.stream.position(self.controller.regs());
        let Some(cursor) = self.cursor.as_mut() else {
            return;
        };
        let first = cursor.next_slot() as usize;
        let passed = cursor.advance(position).min(params.periods) as usize;
        for index in 0..passed {
            let slot = (first + index) % params.periods as usize;
            if core::mem::take(&mut self.queued[slot]) {
                self.silence(slot, 0);
                self.done.push_back(slot);
            }
        }
    }

    pub(super) fn wait_event(&mut self) {
        let Some(irq) = self.claimed.irq else {
            return sys::nap();
        };
        if let Ok(message) = irq.recv_with(&mut self.irq_buf, Some(sys::clock() + 1)) {
            self.handle_irq(&message);
        }
    }

    pub(super) fn service_irq(&mut self) {
        let Some(irq) = self.claimed.irq else {
            return;
        };
        if let Ok(Some(message)) = irq.poll_recv_with(&mut self.irq_buf) {
            self.handle_irq(&message);
        }
    }

    /// Acknowledge one interrupt: only the kernel (slot 0) may send one.
    /// Clearing the stream's status (and the response ring's) deasserts the
    /// line before the kernel is told to unmask it.
    fn handle_irq(&mut self, message: &user::messenger::Message) {
        if message.sender != 0 || user::dev::parse_irq_body(&message.parcel.body).is_none() {
            sys::write_str(&format!("SNDD:IRQ:REJECT sender={}\n", message.sender));
            return;
        }
        self.irqs += 1;
        let regs = self.controller.regs_mut();
        let _ = self.stream.take_status(regs);
        regs.write8(RIRBSTS, 0xFF);
        let _ = user::dev::irq_ack(self.claimed.handle);
    }

    pub(super) fn irq_report(&self) -> (bool, u64) {
        (self.claimed.irq.is_some(), self.irqs)
    }
}
