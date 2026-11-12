//! `os.lazy.audio.v1` (`idl/audio.midl`): request dispatch for the card.
//!
//! One card, one playback stream, one owner: in practice the system mixer,
//! `audiod`, which holds it while it runs. The owner is the kernel-stamped
//! sender of `OpenStream`; nothing in a request body names a caller, so a
//! client cannot act for another. Every failure leaves through
//! the generated `dispatch` as a structured errno.

use alloc::vec::Vec;
use user::trace;

use audiomix::events::{Event, Kind};

use libmessenger::server::{Caller, Fault};
use libmessenger::Buffer;
use user::messenger::audio::wire;
use user::messenger::{errno, Error as MsgError};
use user::sys;
use virtio_snd::params::{self, audio_direction, Request};
use virtio_snd::wire::{direction, PcmInfo};

use super::card::Card;
use super::session::{errno_of, Session};

type Fallible<T> = core::result::Result<T, MsgError>;

pub(super) struct Service {
    card: Card,
    /// The first playback stream, and its index on the device.
    playback: Option<(u32, PcmInfo)>,
    session: Option<Session>,
}

fn err(code: i64) -> MsgError {
    MsgError::Errno(-code)
}

impl Service {
    pub(super) fn new(card: Card, infos: &[PcmInfo]) -> Service {
        let playback = infos
            .iter()
            .enumerate()
            .find(|(_, info)| info.direction == direction::OUTPUT)
            .map(|(index, info)| (index as u32, *info));
        Service {
            card,
            playback,
            session: None,
        }
    }

    /// The card can no longer be driven: the driver should exit.
    pub(super) fn card_lost(&self) -> bool {
        self.card.lost()
    }

    /// Whether the loop should wake every tick to keep the device fed.
    pub(super) fn busy(&self) -> bool {
        self.session.as_ref().is_some_and(Session::is_running)
    }

    /// Keep the device fed and reclaim an abandoned stream. Called on every
    /// wakeup, requested or not.
    pub(super) fn housekeeping(&mut self, events: &mut Vec<Event>) {
        self.card.service_irq();
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let failed = session.pump(&mut self.card, false).is_err();
        session.events(events);
        if failed {
            events.push(Event {
                stream: session.index(),
                kind: Kind::DeviceError,
                frames: session.position(),
            });
        }
        if failed || session.abandoned() {
            if let Some(session) = self.session.take() {
                trace::line!(
                    trace::Channel::Sndd,
                    "RECLAIM stream reclaimed from its owner"
                );
                session.close(&mut self.card);
            }
        }
    }
}

/// The ring `AttachRing` carries is the message's until the generated
/// decoder claims it: a refused request's ring closes when the message drops.
impl wire::Server for Service {
    fn info(&mut self, _: &Caller) -> Result<wire::AudioInfo, Fault> {
        let (_, info) = self.playback.as_ref().ok_or_else(|| err(errno::ENOTSUP))?;
        Ok(wire::AudioInfo {
            streams: 1,
            formats: params::format_bitmap(info),
            rates: params::rate_bitmap(info),
            channels: u32::from(info.channels_max),
            name: self.card.display_name(),
        })
    }

    fn open_stream(
        &mut self,
        cx: &Caller,
        dir: u32,
        format: u32,
        rate: u32,
        channels: u32,
        period_bytes: u32,
    ) -> Result<wire::StreamGrant, Fault> {
        if dir == audio_direction::CAPTURE {
            return Err(err(errno::ENOTSUP).into());
        }
        let (index, info) = self.playback.ok_or_else(|| err(errno::ENOTSUP))?;
        if self.session.is_some() {
            return Err(err(errno::EBUSY).into());
        }
        let request = Request {
            direction: dir,
            format,
            rate_hz: rate,
            channels,
            period_bytes,
        };
        let session = Session::open(&mut self.card, index, &info, cx.sender, &request)
            .map_err(|e| errno_of(&e))?;
        let grant = session.grant();
        self.session = Some(session);
        Ok(wire::StreamGrant {
            stream: index,
            dir: audio_direction::PLAYBACK,
            format: grant.format,
            rate: grant.rate_hz,
            channels: grant.channels,
            period_bytes: grant.period_bytes,
            periods: grant.periods,
        })
    }

    /// `AttachRing`: the decoded request owns the ring; every way out but
    /// success closes it.
    fn attach_ring(&mut self, cx: &Caller, stream: u32, ring: Buffer) -> Result<(), Fault> {
        let outcome = owned(&mut self.session, cx, stream).and_then(|session| session.attach(&ring));
        if outcome.is_err() {
            let _ = sys::buffer_close(ring.handle);
        }
        Ok(outcome?)
    }

    fn commit(&mut self, cx: &Caller, stream: u32, written_frames: u64) -> Result<u64, Fault> {
        Ok(owned(&mut self.session, cx, stream)?.commit(&mut self.card, written_frames)?)
    }

    fn start(&mut self, cx: &Caller, stream: u32) -> Result<bool, Fault> {
        owned(&mut self.session, cx, stream)?.start(&mut self.card)?;
        Ok(true)
    }

    fn stop(&mut self, cx: &Caller, stream: u32) -> Result<bool, Fault> {
        owned(&mut self.session, cx, stream)?.stop(&mut self.card)?;
        Ok(true)
    }

    fn drain(&mut self, cx: &Caller, stream: u32) -> Result<bool, Fault> {
        owned(&mut self.session, cx, stream)?.drain(&mut self.card)?;
        Ok(true)
    }

    fn position(&mut self, cx: &Caller, stream: u32) -> Result<u64, Fault> {
        Ok(owned(&mut self.session, cx, stream)?.position())
    }

    fn set_volume(&mut self, cx: &Caller, stream: u32, gain_q16: u32) -> Result<(), Fault> {
        Ok(owned(&mut self.session, cx, stream)?.set_volume(gain_q16)?)
    }

    fn set_mute(&mut self, cx: &Caller, stream: u32, mute: bool) -> Result<(), Fault> {
        owned(&mut self.session, cx, stream)?.set_mute(mute);
        Ok(())
    }

    fn close_stream(&mut self, cx: &Caller, stream: u32) -> Result<(), Fault> {
        owned(&mut self.session, cx, stream)?;
        if let Some(session) = self.session.take() {
            session.close(&mut self.card);
        }
        Ok(())
    }
}

/// The caller's stream: it must exist, match `stream` and belong to the
/// sender. Touching it resets the abandonment timer. Takes only the session
/// slot so the caller can keep using the card while it holds the result.
fn owned<'a>(
    session: &'a mut Option<Session>,
    cx: &Caller,
    stream: u32,
) -> Fallible<&'a mut Session> {
    let session = session.as_mut().ok_or_else(|| err(errno::EINVAL))?;
    if session.index() != stream {
        return Err(err(errno::EINVAL));
    }
    if session.owner != cx.sender {
        return Err(err(errno::EACCES));
    }
    session.touch();
    Ok(session)
}
