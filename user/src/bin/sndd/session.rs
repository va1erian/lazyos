//! A client's hold on the card: the stream, its owner, the client's sample
//! ring and the commit/consume counters.
//!
//! Everything the client tells us is checked: counters must be monotonic and
//! never run more than one ring ahead of what was consumed, the ring must be
//! at least as long as the grant, and only the owner (the kernel-stamped
//! sender of `OpenStream`) may touch the stream. Client memory is only ever
//! read with a raw copy of a range computed from the driver's own counters, so
//! a client rewriting its ring concurrently can produce noise but never
//! out-of-bounds access.

use core::ptr;

use audiomix::events::{Event, Kind, Starvation};
use audiomix::volume::{StreamVolume, VolumeError};
use libmessenger::BufferDesc;
use user::messenger::{errno, Error as MsgError};
use user::sys;
use virtio_snd::params::{audio_format, Request};
use virtio_snd::wire::PcmInfo;

use super::card::Card;
use super::stream::Stream;

use super::error::Error;

/// Ticks without any owner request, while nothing is playing, before the
/// driver takes the stream back (a dead or wedged client must not hold the
/// card forever).
const IDLE_RECLAIM_TICKS: u64 = 1000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// Opened; not started yet (or not attached).
    Idle,
    Running,
    /// After `Stop`: restartable, frame numbering restarts at 0.
    Stopped,
    /// After `Drain`: only `CloseStream` is left.
    Drained,
}

/// The client's ring as mapped into this task.
struct ClientRing {
    handle: u64,
    base: *const u8,
    bytes: usize,
}

pub(super) struct Session {
    stream: Stream,
    pub(super) owner: u64,
    ring: Option<ClientRing>,
    /// Frames the client has committed / the driver has copied out.
    committed: u64,
    consumed: u64,
    state: State,
    last_active: u64,
    /// `SetVolume` / `SetMute`, applied as each period is staged.
    volume: StreamVolume,
    /// The device ran out of periods while running (issue #453).
    starvation: Starvation,
    /// A `Drain` completed and its event is not out yet.
    drained: bool,
}

type Result<T> = core::result::Result<T, MsgError>;

fn invalid() -> MsgError {
    MsgError::Errno(-errno::EINVAL)
}

fn busy() -> MsgError {
    MsgError::Errno(-errno::EBUSY)
}

/// Map a driver-side failure onto the errno a client sees.
pub(super) fn errno_of(error: &Error) -> MsgError {
    // The client only sees an errno; the serial log keeps the real reason.
    sys::write_str(&alloc::format!(
        "SNDD:ERR {}
",
        error.describe()
    ));
    MsgError::Errno(-match error {
        Error::Params | Error::Range => errno::EINVAL,
        Error::Unsupported | Error::NoStream => errno::ENOTSUP,
        Error::Busy => errno::EBUSY,
        Error::Timeout => errno::ETIMEDOUT,
        Error::NoDevice
        | Error::Dev(_)
        | Error::Virtio(_)
        | Error::Status(_)
        | Error::Messenger(_)
        | Error::Hda(_) => errno::EIO,
    })
}

impl Session {
    pub(super) fn open(
        card: &mut Card,
        index: u32,
        info: &PcmInfo,
        owner: u64,
        request: &Request,
    ) -> core::result::Result<Session, Error> {
        Ok(Session {
            stream: Stream::open(card, index, info, request)?,
            owner,
            ring: None,
            committed: 0,
            consumed: 0,
            state: State::Idle,
            last_active: sys::clock(),
            volume: StreamVolume::new(),
            starvation: Starvation::default(),
            drained: false,
        })
    }

    pub(super) fn grant(&self) -> virtio_snd::params::Grant {
        self.stream.grant
    }

    pub(super) fn index(&self) -> u32 {
        self.stream.index
    }

    /// Note owner activity (resets the reclaim timer).
    pub(super) fn touch(&mut self) {
        self.last_active = sys::clock();
    }

    fn ring_frames(&self) -> u64 {
        let grant = self.stream.grant;
        u64::from(grant.buffer_bytes()) / u64::from(grant.frame_bytes().max(1))
    }

    fn period_frames(&self) -> u64 {
        let grant = self.stream.grant;
        u64::from(grant.period_bytes) / u64::from(grant.frame_bytes().max(1))
    }

    /// Map the client's ring. `desc` is the request's declared range; the
    /// kernel already checked it lies inside the shared object.
    pub(super) fn attach(&mut self, handle: u64, desc: &BufferDesc) -> Result<()> {
        if self.ring.is_some() {
            return Err(busy());
        }
        let needed = u64::from(self.stream.grant.buffer_bytes());
        if desc.len < needed {
            return Err(invalid());
        }
        let offset = usize::try_from(desc.offset).map_err(|_| invalid())?;
        let bytes = usize::try_from(desc.len).map_err(|_| invalid())?;
        let va = sys::buffer_map(handle)
            .map(|(va, _)| va)
            .map_err(MsgError::Errno)?;
        let base = (va as usize).checked_add(offset).ok_or_else(invalid)? as *const u8;
        self.ring = Some(ClientRing {
            handle,
            base,
            bytes,
        });
        Ok(())
    }

    /// Accept the client's write position; returns frames consumed so far.
    pub(super) fn commit(&mut self, card: &mut Card, written: u64) -> Result<u64> {
        if self.ring.is_none() || matches!(self.state, State::Drained) {
            return Err(invalid());
        }
        // Monotonic, and no further ahead than the ring can hold beyond what
        // the driver already copied out.
        if written < self.committed || written - self.consumed > self.ring_frames() {
            return Err(invalid());
        }
        self.committed = written;
        self.pump(card, false)?;
        Ok(self.consumed)
    }

    pub(super) fn start(&mut self, card: &mut Card) -> Result<()> {
        if self.ring.is_none() {
            return Err(invalid());
        }
        match self.state {
            State::Running => return Ok(()),
            State::Drained => return Err(busy()),
            State::Stopped => self.stream.program(card).map_err(|e| errno_of(&e))?,
            State::Idle => {}
        }
        self.stream.start(card).map_err(|e| errno_of(&e))?;
        self.state = State::Running;
        self.pump(card, false)?;
        Ok(())
    }

    /// Stop now, discarding committed periods not yet queued.
    pub(super) fn stop(&mut self, card: &mut Card) -> Result<()> {
        if self.state != State::Running {
            return Ok(());
        }
        self.stream.halt(card).map_err(|e| errno_of(&e))?;
        self.committed = 0;
        self.consumed = 0;
        self.state = State::Stopped;
        self.starvation.reset();
        Ok(())
    }

    /// Play everything committed, then stop for good.
    pub(super) fn drain(&mut self, card: &mut Card) -> Result<()> {
        match self.state {
            State::Drained => return Ok(()),
            State::Running => {}
            _ => return Err(invalid()),
        }
        let mut deadline = sys::clock() + STALL_TICKS;
        loop {
            self.pump(card, true)?;
            if self.consumed == self.committed && !self.stream.has_busy() {
                break;
            }
            if self.stream.reap(card).map_err(|e| errno_of(&e))? {
                deadline = sys::clock() + STALL_TICKS;
            } else if sys::clock() >= deadline {
                return Err(MsgError::Errno(-errno::ETIMEDOUT));
            } else {
                card.idle();
            }
        }
        self.stream.halt(card).map_err(|e| errno_of(&e))?;
        self.state = State::Drained;
        self.drained = true;
        Ok(())
    }

    pub(super) fn position(&self) -> u64 {
        self.stream.frames_done
    }

    /// Scale what is staged from now on. Only `S16Le` can be scaled here; any
    /// other granted format accepts unity alone (`ENOTSUP` otherwise).
    pub(super) fn set_volume(&mut self, gain_q16: u32) -> Result<()> {
        let s16le = self.stream.grant.format == audio_format::S16_LE;
        self.volume
            .set_volume(gain_q16, s16le)
            .map_err(|error| match error {
                VolumeError::OutOfRange => invalid(),
                VolumeError::Unsupported => MsgError::Errno(-errno::ENOTSUP),
            })
    }

    /// Stage silence instead of the client's samples; the stream keeps
    /// consuming and its position keeps moving.
    pub(super) fn set_mute(&mut self, mute: bool) {
        self.volume.set_mute(mute);
    }

    /// Copy committed periods into free DMA slots and queue them. With
    /// `flush` a final short period is sent too.
    pub(super) fn pump(&mut self, card: &mut Card, flush: bool) -> Result<()> {
        if self.state != State::Running {
            return Ok(());
        }
        self.stream.reap(card).map_err(|e| errno_of(&e))?;
        let period_frames = self.period_frames();
        let frame_bytes = self.stream.grant.frame_bytes() as usize;
        let period_bytes = self.stream.grant.period_bytes as usize;
        let periods = u64::from(self.stream.grant.periods);
        while self.committed > self.consumed {
            let available = self.committed - self.consumed;
            let take = if available >= period_frames {
                period_frames
            } else if flush {
                available
            } else {
                break;
            };
            let Some(slot) = self.stream.try_slot() else {
                break;
            };
            let ring = self.ring.as_ref().ok_or_else(invalid)?;
            // Ring position of the next unconsumed period. `consumed` only
            // ever moves by whole periods except for the final short one, so
            // it stays period aligned while more periods follow.
            let index = (self.consumed / period_frames % periods) as usize;
            let len = take as usize * frame_bytes;
            let src_offset = index * period_bytes;
            if src_offset + len > ring.bytes || len > period_bytes {
                return Err(invalid());
            }
            let s16le = self.stream.grant.format == audio_format::S16_LE;
            let volume = self.volume;
            let dst = self.stream.slot_bytes(slot).map_err(|e| errno_of(&e))?;
            // SAFETY: `src_offset + len <= ring.bytes`, the mapped extent the
            // kernel validated for this buffer, and `len <= period_bytes` is
            // the destination slot's length. The regions belong to different
            // mappings, so they cannot overlap; the client may write the
            // source concurrently, which yields noise, not UB (raw copy, no
            // reference to client memory is ever formed).
            unsafe { ptr::copy_nonoverlapping(ring.base.add(src_offset), dst.as_mut_ptr(), len) };
            // Volume is applied to the driver's own copy, so a client
            // rewriting its ring cannot undo it.
            volume.stage(&mut dst[..len], s16le);
            self.stream
                .submit_slot(card, slot, take as u32)
                .map_err(|e| errno_of(&e))?;
            self.consumed += take;
        }
        Ok(())
    }

    /// Whether the driver should reclaim this stream: the owner has been
    /// silent for a long time and nothing is left to play. A stream that was
    /// never started has nothing playing even if frames were committed to it
    /// (`pump` does nothing before `Start`), so those pending frames must not
    /// keep it alive: otherwise an owner that commits and then dies would hold
    /// the card (every later `OpenStream` gets `EBUSY`) forever.
    pub(super) fn abandoned(&self) -> bool {
        let idle = self.state == State::Idle
            || (self.committed == self.consumed && !self.stream.has_busy());
        idle && sys::clock().saturating_sub(self.last_active) > IDLE_RECLAIM_TICKS
    }

    /// The stream's events since the last look (issue #453): one `Underrun`
    /// when the running device has played every queued period and nothing
    /// committed is left to queue (after it played something), and one
    /// `Drained` when a `Drain` completed.
    pub(super) fn events(&mut self, out: &mut alloc::vec::Vec<Event>) {
        let frames = self.stream.frames_done;
        let starved =
            self.consumed > 0 && self.committed == self.consumed && !self.stream.has_busy();
        if self
            .starvation
            .observe(self.state == State::Running, starved)
        {
            out.push(event(self.index(), Kind::Underrun, frames));
        }
        if core::mem::take(&mut self.drained) {
            out.push(event(self.index(), Kind::Drained, frames));
        }
    }

    /// Whether the driver needs frequent wakeups to keep the device fed.
    pub(super) fn is_running(&self) -> bool {
        self.state == State::Running
    }

    /// Stop the device, drop the client's ring and give the DMA staging back
    /// to the card for the next stream.
    pub(super) fn close(mut self, card: &mut Card) {
        match self.state {
            State::Running => {
                let _ = self.stream.halt(card);
            }
            // Prepared but never started: hand the parameters back.
            State::Idle => {
                let _ = self.stream.release(card);
            }
            State::Stopped | State::Drained => {}
        }
        if let Some(ring) = self.ring.take() {
            let _ = sys::buffer_close(ring.handle);
        }
        card.give_back(self.stream.into_region());
    }
}

/// Ticks without progress before a `Drain` gives up.
const STALL_TICKS: u64 = 500;

fn event(stream: u32, kind: Kind, frames: u64) -> Event {
    Event {
        stream,
        kind,
        frames,
    }
}
