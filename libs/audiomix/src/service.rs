//! The mixer's wire layer: one decoded request in, one [`Outcome`] out.
//!
//! `audiod` serves two interfaces under `os.lazy.audio`. `os.lazy.audio.v1`
//! is the stream protocol every application speaks (the same one the card
//! driver serves, so a client works against either); `os.lazy.audio.mixer.v1`
//! is the control panel. Both are decoded with the `midlc`-generated codecs
//! and dispatched here, without syscalls, so `audiod` and the client
//! library's host tests run exactly the same server logic.
//!
//! The sender is the kernel-stamped task id; nothing in a request body names a
//! caller. A transferred ring arrives as `ring` and is adopted only by a
//! well-formed `AttachRing`; on every other path it is dropped, which is how
//! `audiod` closes buffers attached to the wrong method.

use alloc::vec::Vec;

use messenger_generated::os_lazy_audio_mixer_v1 as control;
use messenger_generated::os_lazy_audio_v1 as audio;
use virtio_snd::params::{audio_format, Request as StreamRequest, RATES};

use crate::gain::Gain;
use crate::{MixError, Mixer, Ring, MIX_CHANNELS};

/// The errno values this layer answers with (positive, Linux numbering).
pub mod errno {
    pub const ENOENT: i64 = 2;
    pub const EACCES: i64 = 13;
    pub const EBUSY: i64 = 16;
    pub const ENODEV: i64 = 19;
    pub const EINVAL: i64 = 22;
    pub const ENOTSUP: i64 = 95;
    pub const ECANCELED: i64 = 125;
}

/// What to send back for one request.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The encoded reply body.
    Reply(Vec<u8>),
    /// Refuse with this (positive) errno.
    Refuse(i64),
    /// `Drain` of this stream is under way: answer with [`drained_reply`]
    /// once [`Mixer::played`] reports it, or [`errno::ECANCELED`] if the
    /// stream is stopped or closed first.
    DrainPending(u32),
}

/// One request as received.
pub struct Request<'a, R> {
    pub interface: u64,
    pub method: u32,
    pub body: &'a [u8],
    /// The kernel-stamped sender.
    pub sender: u64,
    /// The request's transferred buffer, mapped, when it carried one.
    pub ring: Option<R>,
    pub now: u64,
}

/// The reply body of a completed `Drain`.
pub fn drained_reply() -> Vec<u8> {
    audio::encode_drain_reply(&audio::DrainReply { ok: true }).unwrap_or_default()
}

/// Dispatch one request. `mixer` is `None` while no card is attached: stream
/// calls then fail with `ENODEV` and the control panel reports no card.
pub fn handle<R: Ring>(mixer: Option<&mut Mixer<R>>, request: Request<'_, R>) -> Outcome {
    let result = match request.interface {
        audio::INTERFACE_ID => match mixer {
            Some(mixer) => stream_call(mixer, request),
            None => Err(errno::ENODEV),
        },
        control::INTERFACE_ID => control_call(mixer, request),
        _ => Err(errno::EINVAL),
    };
    result.unwrap_or_else(Outcome::Refuse)
}

type Answer = Result<Outcome, i64>;

/// Map an engine refusal; an unknown stream is `EINVAL` on the stream
/// interface (as the driver answers) and `ENOENT` on the control panel.
fn errno_of(error: MixError, not_found: i64) -> i64 {
    match error {
        MixError::Invalid => errno::EINVAL,
        MixError::NotFound => not_found,
        MixError::Access => errno::EACCES,
        MixError::Busy => errno::EBUSY,
        MixError::Unsupported => errno::ENOTSUP,
    }
}

fn stream_err(error: MixError) -> i64 {
    errno_of(error, errno::EINVAL)
}

fn reply<T>(encoded: Result<Vec<u8>, T>) -> Answer {
    encoded.map(Outcome::Reply).map_err(|_| errno::EINVAL)
}

fn empty() -> Answer {
    Ok(Outcome::Reply(Vec::new()))
}

fn stream_call<R: Ring>(mixer: &mut Mixer<R>, request: Request<'_, R>) -> Answer {
    let Request {
        method,
        body,
        sender,
        ring,
        now,
        ..
    } = request;
    let bad = |_| errno::EINVAL;
    match method {
        audio::METHOD_INFO => reply(audio::encode_info_reply(&audio::InfoReply {
            info: info(mixer),
        })),
        audio::METHOD_OPENSTREAM => {
            let args = audio::decode_open_stream_args(body).map_err(bad)?;
            let wanted = StreamRequest {
                direction: args.dir,
                format: args.format,
                rate_hz: args.rate,
                channels: args.channels,
                period_bytes: args.period_bytes,
            };
            let (stream, grant) = mixer.open(sender, &wanted, now).map_err(stream_err)?;
            reply(audio::encode_open_stream_reply(&audio::OpenStreamReply {
                grant: audio::StreamGrant {
                    stream,
                    dir: audio::DIRECTION_PLAYBACK,
                    format: grant.format,
                    rate: grant.rate_hz,
                    channels: grant.channels,
                    period_bytes: grant.period_bytes,
                    periods: grant.periods,
                },
            }))
        }
        audio::METHOD_ATTACHRING => {
            let args = audio::decode_attach_ring_args(body).map_err(bad)?;
            let ring = ring.ok_or(errno::EINVAL)?;
            mixer
                .attach(args.stream, sender, ring, now)
                .map_err(stream_err)?;
            empty()
        }
        audio::METHOD_COMMIT => {
            let args = audio::decode_commit_args(body).map_err(bad)?;
            let consumed = mixer
                .commit(args.stream, sender, args.written_frames, now)
                .map_err(stream_err)?;
            reply(audio::encode_commit_reply(&audio::CommitReply { consumed }))
        }
        audio::METHOD_START => {
            let args = audio::decode_start_args(body).map_err(bad)?;
            mixer.start(args.stream, sender, now).map_err(stream_err)?;
            reply(audio::encode_start_reply(&audio::StartReply { ok: true }))
        }
        audio::METHOD_STOP => {
            let args = audio::decode_stop_args(body).map_err(bad)?;
            mixer.stop(args.stream, sender, now).map_err(stream_err)?;
            reply(audio::encode_stop_reply(&audio::StopReply { ok: true }))
        }
        audio::METHOD_DRAIN => {
            let args = audio::decode_drain_args(body).map_err(bad)?;
            if mixer.drain(args.stream, sender, now).map_err(stream_err)? {
                Ok(Outcome::Reply(drained_reply()))
            } else {
                Ok(Outcome::DrainPending(args.stream))
            }
        }
        audio::METHOD_POSITION => {
            let args = audio::decode_position_args(body).map_err(bad)?;
            let frames = mixer
                .position(args.stream, sender, now)
                .map_err(stream_err)?;
            reply(audio::encode_position_reply(&audio::PositionReply {
                frames,
            }))
        }
        audio::METHOD_CLOSESTREAM => {
            let args = audio::decode_close_stream_args(body).map_err(bad)?;
            mixer.close(args.stream, sender, now).map_err(stream_err)?;
            empty()
        }
        audio::METHOD_SETVOLUME => {
            let args = audio::decode_set_volume_args(body).map_err(bad)?;
            mixer
                .set_volume(args.stream, Some(sender), args.gain_q16, now)
                .map_err(stream_err)?;
            empty()
        }
        audio::METHOD_SETMUTE => {
            let args = audio::decode_set_mute_args(body).map_err(bad)?;
            mixer
                .set_mute(args.stream, Some(sender), args.mute, now)
                .map_err(stream_err)?;
            empty()
        }
        _ => Err(errno::EINVAL),
    }
}

/// What the mixer accepts: `S16Le` at every IDL rate, up to stereo.
fn info<R: Ring>(mixer: &Mixer<R>) -> audio::AudioInfo {
    audio::AudioInfo {
        streams: mixer.config().max_streams as u32,
        formats: 1 << audio_format::S16_LE,
        rates: (1u32 << RATES.len()) - 1,
        channels: MIX_CHANNELS as u32,
    }
}

fn control_call<R: Ring>(mixer: Option<&mut Mixer<R>>, request: Request<'_, R>) -> Answer {
    let Request {
        method, body, now, ..
    } = request;
    let bad = |_| errno::EINVAL;
    let not_found = |error| errno_of(error, errno::ENOENT);
    match method {
        control::METHOD_LISTSTREAMS => {
            let streams = mixer
                .map(|mixer| mixer.statuses().map(status).collect())
                .unwrap_or_default();
            reply(control::encode_list_streams_reply(
                &control::ListStreamsReply { streams },
            ))
        }
        control::METHOD_GETMASTER => {
            reply(control::encode_get_master_reply(&control::GetMasterReply {
                master: master(mixer.as_deref()),
            }))
        }
        control::METHOD_SETMASTER => {
            let args = control::decode_set_master_args(body).map_err(bad)?;
            let mixer = mixer.ok_or(errno::ENODEV)?;
            mixer
                .set_master(args.gain_q16, args.mute)
                .map_err(not_found)?;
            empty()
        }
        control::METHOD_SETSTREAMVOLUME => {
            let args = control::decode_set_stream_volume_args(body).map_err(bad)?;
            let mixer = mixer.ok_or(errno::ENOENT)?;
            // Validate the gain before touching anything, so a bad gain
            // never half-applies (mute set, volume refused).
            Gain::new(args.gain_q16).ok_or(errno::EINVAL)?;
            mixer
                .set_volume(args.stream, None, args.gain_q16, now)
                .map_err(not_found)?;
            mixer
                .set_mute(args.stream, None, args.mute, now)
                .map_err(not_found)?;
            empty()
        }
        _ => Err(errno::EINVAL),
    }
}

fn status(status: crate::Status) -> control::StreamStatus {
    control::StreamStatus {
        stream: status.id,
        owner: status.owner,
        state: status.state.ordinal(),
        rate: status.rate,
        channels: status.channels,
        gain_q16: status.gain.q16(),
        mute: status.muted,
        frames: status.played,
        underruns: status.underruns,
    }
}

fn master<R: Ring>(mixer: Option<&Mixer<R>>) -> control::Master {
    match mixer {
        Some(mixer) => {
            let (gain, mute) = mixer.master();
            control::Master {
                gain_q16: gain.q16(),
                mute,
                card: true,
                rate: mixer.config().rate,
                channels: MIX_CHANNELS as u32,
                streams: mixer.len() as u32,
                max_streams: mixer.config().max_streams as u32,
            }
        }
        None => control::Master {
            gain_q16: Gain::UNITY.q16(),
            mute: false,
            card: false,
            rate: 0,
            channels: 0,
            streams: 0,
            max_streams: 0,
        },
    }
}
