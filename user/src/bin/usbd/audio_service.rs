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

use libmessenger::server::{Caller, Fault};
use libmessenger::Buffer;
use user::messenger::usb_audio::{self as api, wire};
use user::messenger::{self, errno, registry, Endpoint, Error as MsgError};
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
                        trace::line!(
                            trace::Channel::Usbd,
                            "AUDIO:FAIL register {}",
                            error.message()
                        );
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
            let reply = wire::dispatch(&mut Handler(target), &message);
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

/// The bound audio function, answering `os.lazy.usb.audio.v1` (the generated
/// server); `None` while no function is bound.
struct Handler<'a>(Option<(&'a mut Hc, &'a mut Device, &'a mut AudioFn)>);

type Target<'h> = (&'h mut Hc, &'h mut Device, &'h mut AudioFn);

impl Handler<'_> {
    /// The function, for `_snd` and root alone (the kernel-stamped uid).
    fn target(&mut self, cx: &Caller) -> Result<Target<'_>, Fault> {
        let Some((hc, device, audio)) = self.0.as_mut() else {
            return Err(err(errno::ENODEV).into());
        };
        if cx.uid != 0 && cx.uid != sndpolicy::SND_UID {
            return Err(err(errno::EACCES).into());
        }
        Ok((&mut **hc, &mut **device, &mut **audio))
    }
}

impl wire::Server for Handler<'_> {
    fn describe(&mut self, cx: &Caller) -> Result<wire::Device, Fault> {
        let (_, _, audio) = self.target(cx)?;
        Ok(audio.describe())
    }

    fn attach(&mut self, cx: &Caller, staging: Buffer) -> Result<(), Fault> {
        // The dispatch already claimed the buffer; every refusal releases it.
        let (hc, device, audio) = match self.target(cx) {
            Ok(target) => target,
            Err(fault) => {
                let _ = sys::buffer_close(staging.handle);
                return Err(fault);
            }
        };
        let Some(staging_map) = Staging::map(staging.handle, staging.offset, staging.len) else {
            let _ = sys::buffer_close(staging.handle);
            return Err(err(errno::EINVAL).into());
        };
        audio.attach(hc, device, staging_map);
        Ok(())
    }

    fn configure(&mut self, cx: &Caller, rate: u32) -> Result<u32, Fault> {
        let (hc, device, audio) = self.target(cx)?;
        Ok(audio.configure(hc, device, rate).map_err(refusal)?)
    }

    fn start(&mut self, cx: &Caller) -> Result<(), Fault> {
        let (hc, device, audio) = self.target(cx)?;
        Ok(audio.start(hc, device).map_err(refusal)?)
    }

    fn stop(&mut self, cx: &Caller) -> Result<(), Fault> {
        let (hc, device, audio) = self.target(cx)?;
        audio.stop(hc, device);
        Ok(())
    }

    fn release(&mut self, cx: &Caller) -> Result<(), Fault> {
        let (hc, device, audio) = self.target(cx)?;
        audio.release(hc, device);
        Ok(())
    }

    fn submit(&mut self, cx: &Caller, _slot: u32, offset: u32, len: u32) -> Result<(), Fault> {
        let (hc, device, audio) = self.target(cx)?;
        audio.submit(offset, len).map_err(refusal)?;
        audio.pump(hc, device);
        Ok(())
    }

    fn reap(&mut self, cx: &Caller) -> Result<wire::Progress, Fault> {
        let (_, _, audio) = self.target(cx)?;
        Ok(audio.progress())
    }
}
