//! `os.lazy.audio.v1` (`idl/audio.midl`): request dispatch for the card.
//!
//! One card, one playback stream, one owner: in practice the system mixer,
//! `audiod`, which holds it while it runs. The owner is the kernel-stamped
//! sender of `OpenStream`; nothing in a request body names a caller, so a
//! client cannot act for another. Every failure leaves through
//! [`user::messenger::services::error_reply`] as a structured errno.

use alloc::vec::Vec;

use audiomix::events::{Event, Kind};

use user::messenger::audio::{self as api, wire};
use user::messenger::{errno, Error as MsgError, Message, Parcel};
use user::sys;
use virtio_snd::params::{self, audio_direction, Request};
use virtio_snd::wire::{direction, PcmInfo};

use super::card::Card;
use super::session::{errno_of, Session};

type Result<T> = core::result::Result<T, MsgError>;

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
                sys::write_str("SNDD:RECLAIM stream reclaimed from its owner\n");
                session.close(&mut self.card);
            }
        }
    }

    /// Route one request. `Ok` is the reply; `Err` becomes the error reply.
    pub(super) fn dispatch(&mut self, message: &Message) -> Result<Parcel> {
        // Whatever the request transferred lands in this task's handle table
        // before we look at it, so every path out, success or refusal, must
        // close what was not adopted: a client that can resolve this service
        // could otherwise fill the table with malformed requests.
        let mut adopted = false;
        let result = self.route(message, &mut adopted);
        discard_transfers(message, adopted);
        result
    }

    fn route(&mut self, message: &Message, ring_adopted: &mut bool) -> Result<Parcel> {
        if message.interface_id() != api::INTERFACE {
            return Err(err(errno::EINVAL));
        }
        let method = message.method();
        // Exactly what `audio.midl` declares for the method, or nothing is
        // adopted (`dispatch` closes the objects).
        if !message.carries(wire::request_transfers(method)) {
            return Err(err(errno::EINVAL));
        }
        let body = &message.parcel.body;
        let reply = match method {
            wire::METHOD_INFO => self.info()?,
            wire::METHOD_OPENSTREAM => self.open_stream(message)?,
            wire::METHOD_ATTACHRING => {
                let args = wire::decode_attach_ring_args(body).map_err(MsgError::Parcel)?;
                self.attach_ring(message, args.stream)?;
                *ring_adopted = true;
                Vec::new()
            }
            wire::METHOD_COMMIT => {
                let args = wire::decode_commit_args(body).map_err(MsgError::Parcel)?;
                let consumed = owned(&mut self.session, message, args.stream)?
                    .commit(&mut self.card, args.written_frames)?;
                wire::encode_commit_reply(&wire::CommitReply { consumed })
                    .map_err(MsgError::Parcel)?
            }
            wire::METHOD_START => {
                let args = wire::decode_start_args(body).map_err(MsgError::Parcel)?;
                owned(&mut self.session, message, args.stream)?.start(&mut self.card)?;
                wire::encode_start_reply(&wire::StartReply { ok: true })
                    .map_err(MsgError::Parcel)?
            }
            wire::METHOD_STOP => {
                let args = wire::decode_stop_args(body).map_err(MsgError::Parcel)?;
                owned(&mut self.session, message, args.stream)?.stop(&mut self.card)?;
                wire::encode_stop_reply(&wire::StopReply { ok: true }).map_err(MsgError::Parcel)?
            }
            wire::METHOD_DRAIN => {
                let args = wire::decode_drain_args(body).map_err(MsgError::Parcel)?;
                owned(&mut self.session, message, args.stream)?.drain(&mut self.card)?;
                wire::encode_drain_reply(&wire::DrainReply { ok: true })
                    .map_err(MsgError::Parcel)?
            }
            wire::METHOD_POSITION => {
                let args = wire::decode_position_args(body).map_err(MsgError::Parcel)?;
                let frames = owned(&mut self.session, message, args.stream)?.position();
                wire::encode_position_reply(&wire::PositionReply { frames })
                    .map_err(MsgError::Parcel)?
            }
            wire::METHOD_SETVOLUME => {
                let args = wire::decode_set_volume_args(body).map_err(MsgError::Parcel)?;
                owned(&mut self.session, message, args.stream)?.set_volume(args.gain_q16)?;
                Vec::new()
            }
            wire::METHOD_SETMUTE => {
                let args = wire::decode_set_mute_args(body).map_err(MsgError::Parcel)?;
                owned(&mut self.session, message, args.stream)?.set_mute(args.mute);
                Vec::new()
            }
            wire::METHOD_CLOSESTREAM => {
                let args = wire::decode_close_stream_args(body).map_err(MsgError::Parcel)?;
                owned(&mut self.session, message, args.stream)?;
                if let Some(session) = self.session.take() {
                    session.close(&mut self.card);
                }
                Vec::new()
            }
            _ => return Err(err(errno::EINVAL)),
        };
        Ok(api::parcel(method, reply))
    }

    fn info(&self) -> Result<Vec<u8>> {
        let (_, info) = self.playback.as_ref().ok_or_else(|| err(errno::ENOTSUP))?;
        wire::encode_info_reply(&wire::InfoReply {
            info: wire::AudioInfo {
                streams: 1,
                formats: params::format_bitmap(info),
                rates: params::rate_bitmap(info),
                channels: u32::from(info.channels_max),
            },
        })
        .map_err(MsgError::Parcel)
    }

    fn open_stream(&mut self, message: &Message) -> Result<Vec<u8>> {
        let args = wire::decode_open_stream_args(&message.parcel.body).map_err(MsgError::Parcel)?;
        if args.dir == audio_direction::CAPTURE {
            return Err(err(errno::ENOTSUP));
        }
        let (index, info) = self.playback.ok_or_else(|| err(errno::ENOTSUP))?;
        if self.session.is_some() {
            return Err(err(errno::EBUSY));
        }
        let request = Request {
            direction: args.dir,
            format: args.format,
            rate_hz: args.rate,
            channels: args.channels,
            period_bytes: args.period_bytes,
        };
        let session = Session::open(&mut self.card, index, &info, message.sender, &request)
            .map_err(|e| errno_of(&e))?;
        let grant = session.grant();
        self.session = Some(session);
        wire::encode_open_stream_reply(&wire::OpenStreamReply {
            grant: wire::StreamGrant {
                stream: index,
                dir: audio_direction::PLAYBACK,
                format: grant.format,
                rate: grant.rate_hz,
                channels: grant.channels,
                period_bytes: grant.period_bytes,
                periods: grant.periods,
            },
        })
        .map_err(MsgError::Parcel)
    }

    fn attach_ring(&mut self, message: &Message, stream: u32) -> Result<()> {
        // The ring is the request's first transferred buffer; without one there
        // is nothing to attach.
        let desc = message
            .parcel
            .buffers
            .first()
            .ok_or_else(|| err(errno::EINVAL))?;
        if !message.carries(wire::ATTACH_RING_TRANSFERS) {
            return Err(err(errno::EINVAL));
        }
        owned(&mut self.session, message, stream)?.attach(message.first_buffer, desc)
    }
}

/// Close whatever a request transferred and the driver did not adopt: the ring
/// buffer unless `AttachRing` took it, and any endpoint (no method uses one).
/// The kernel surfaces only the first of each kind, so a request carrying
/// several still leaves the extras open until the client's own quotas stop it.
fn discard_transfers(message: &Message, buffer_adopted: bool) {
    if message.buffers > 0 && !buffer_adopted {
        let _ = sys::display_close_buffer(message.first_buffer);
    }
    if message.handles > 0 {
        let _ = user::messenger::Endpoint::from_raw(message.first_handle).close();
    }
}

/// The caller's stream: it must exist, match `stream` and belong to the
/// sender. Touching it resets the abandonment timer. Takes only the session
/// slot so the caller can keep using the card while it holds the result.
fn owned<'a>(
    session: &'a mut Option<Session>,
    message: &Message,
    stream: u32,
) -> Result<&'a mut Session> {
    let session = session.as_mut().ok_or_else(|| err(errno::EINVAL))?;
    if session.index() != stream {
        return Err(err(errno::EINVAL));
    }
    if session.owner != message.sender {
        return Err(err(errno::EACCES));
    }
    session.touch();
    Ok(session)
}
