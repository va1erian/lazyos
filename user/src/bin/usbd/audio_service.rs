//! `os.lazy.usb.audio.v1` (`idl/usb_audio.midl`): the audio function as
//! `sndd usb=1` drives it.
//!
//! The name `os.lazy.usb.audio` exists exactly while an audio function is
//! bound: [`Server::sync`] registers it when one appears and withdraws it
//! (closing the endpoint, so a connected `sndd` sees its peer die) when it
//! goes. Requests are taken without waiting on every loop pass; the main
//! loop's idle park includes the endpoint. Only `_snd` and root may call.

use alloc::vec;
use alloc::vec::Vec;
use user::trace;

use user::messenger::usb_audio::{self as api, wire};
use user::messenger::{self, errno, registry, services, Endpoint, Error as MsgError, Message, Parcel};
use user::sys;

use super::audio::{AudioFn, Refusal};
use super::audio_ring::Staging;
use super::bus::Controller;
use super::device::Device;
use super::hc::Hc;

/// Requests served per loop pass at most (the isochronous ring comes first).
const BATCH: usize = 16;
/// Ticks before registering the name again after the registry refused it.
const REGISTER_RETRY_TICKS: u64 = 100;

pub(super) struct Server {
    /// The published and the serving end, while the name is registered.
    ends: Option<(Endpoint, Endpoint)>,
    buffer: Vec<u8>,
    /// No register attempt before this tick (the last one was refused).
    retry_at: u64,
}

type Result<T> = core::result::Result<T, MsgError>;

fn err(code: i64) -> MsgError {
    MsgError::Errno(-code)
}

impl Server {
    pub(super) fn new() -> Server {
        Server {
            ends: None,
            buffer: vec![0u8; messenger::DEFAULT_BUFFER],
            retry_at: 0,
        }
    }

    /// The serving end, for the idle wait.
    pub(super) fn endpoint(&self) -> Option<Endpoint> {
        self.ends.map(|(_, server)| server)
    }

    /// Register or withdraw the name to match whether a device is bound.
    pub(super) fn sync(&mut self, controllers: &mut [Controller]) {
        let bound = controllers.iter_mut().any(|c| c.audio().is_some());
        match (bound, self.ends) {
            (true, None) => {
                if sys::clock() < self.retry_at {
                    return;
                }
                self.retry_at = sys::clock() + REGISTER_RETRY_TICKS;
                let Ok((published, server)) = messenger::create_pair() else {
                    return;
                };
                match registry::register(api::NAME, &published, &[api::INTERFACE], 0) {
                    Ok(()) => {
                        trace::line!(trace::Channel::Usbd, "AUDIO:SERVE name={}", api::NAME);
                        self.ends = Some((published, server));
                    }
                    Err(error) => {
                        trace::line!(trace::Channel::Usbd, "AUDIO:FAIL register {}", error.message());
                        let _ = published.close();
                        let _ = server.close();
                    }
                }
            }
            (false, Some((published, server))) => {
                let _ = registry::unregister(api::NAME);
                let _ = published.close();
                let _ = server.close();
                self.ends = None;
                trace::line!(trace::Channel::Usbd, "AUDIO:WITHDRAW name={}", api::NAME);
            }
            _ => {}
        }
    }

    /// Answer the queued requests; whether there were any.
    pub(super) fn serve(&mut self, controllers: &mut [Controller]) -> bool {
        let Some((_, server)) = self.ends else {
            return false;
        };
        let mut served = false;
        for _ in 0..BATCH {
            let message = match server.poll_recv_with(&mut self.buffer) {
                Ok(Some(message)) => message,
                _ => break,
            };
            served = true;
            let target = controllers.iter_mut().find_map(Controller::audio);
            let reply = match target {
                Some((hc, device, audio)) => dispatch(hc, device, audio, &message),
                None => Err(err(errno::ENODEV)),
            }
            .unwrap_or_else(|error| {
                services::error_reply(message.interface_id(), message.method(), error)
            });
            if let Some(txn) = message.txn {
                let _ = server.reply_or_drop(txn, &reply);
            }
        }
        served
    }
}

fn refusal(refusal: Refusal) -> MsgError {
    err(match refusal {
        Refusal::Invalid => errno::EINVAL,
        Refusal::NoRoom => errno::ENOSPC,
        Refusal::Device => errno::EIO,
    })
}

fn dispatch(hc: &mut Hc, device: &mut Device, audio: &mut AudioFn, message: &Message) -> Result<Parcel> {
    if message.interface_id() != api::INTERFACE {
        return Err(err(errno::EINVAL));
    }
    let uid = message.caller.uid;
    if uid != 0 && uid != sndpolicy::SND_UID {
        return Err(err(errno::EACCES));
    }
    let body = &message.parcel.body;
    let method = message.method();
    let reply = match method {
        wire::METHOD_DESCRIBE => wire::encode_describe_reply(&wire::DescribeReply {
            device: audio.describe(),
        })
        .map_err(MsgError::Parcel)?,
        wire::METHOD_ATTACH => {
            let args = message.decode(wire::decode_attach_args)?;
            let buffer = args.staging;
            let Some(staging) = Staging::map(buffer.handle, buffer.offset, buffer.len) else {
                let _ = sys::buffer_close(buffer.handle);
                return Err(err(errno::EINVAL));
            };
            audio.attach(hc, device, staging);
            Vec::new()
        }
        wire::METHOD_CONFIGURE => {
            let args = wire::decode_configure_args(body).map_err(MsgError::Parcel)?;
            let granted = audio.configure(hc, device, args.rate).map_err(refusal)?;
            wire::encode_configure_reply(&wire::ConfigureReply { granted }).map_err(MsgError::Parcel)?
        }
        wire::METHOD_START => {
            audio.start(hc, device).map_err(refusal)?;
            Vec::new()
        }
        wire::METHOD_STOP => {
            audio.stop(hc, device);
            Vec::new()
        }
        wire::METHOD_RELEASE => {
            audio.release(hc, device);
            Vec::new()
        }
        wire::METHOD_SUBMIT => {
            let args = wire::decode_submit_args(body).map_err(MsgError::Parcel)?;
            audio.submit(args.offset, args.len).map_err(refusal)?;
            audio.pump(hc, device);
            Vec::new()
        }
        wire::METHOD_REAP => wire::encode_reap_reply(&wire::ReapReply {
            progress: audio.progress(),
        })
        .map_err(MsgError::Parcel)?,
        _ => return Err(err(errno::EINVAL)),
    };
    Ok(api::parcel(method, reply, Vec::new()))
}
