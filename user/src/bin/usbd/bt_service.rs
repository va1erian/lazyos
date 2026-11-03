//! `os.lazy.usb.bt.v1` (`idl/usb_bt.midl`): the Bluetooth controller as
//! `btd` drives it.
//!
//! The name `os.lazy.usb.bt` exists exactly while a controller is bound:
//! [`Server::sync`] registers it when one appears and withdraws it (closing
//! the endpoint, so a connected `btd` sees its peer die) when it goes.
//! Requests are taken without waiting on every loop pass; the main loop's
//! idle park includes the endpoint, which is also where `Kick` arrives.
//! Only `_bt` and root may call (`btpolicy`): anyone else gets `EACCES`.

use alloc::vec;
use alloc::vec::Vec;
use user::trace;

use user::messenger::usb_bt::{self as api, wire};
use user::messenger::{
    self, errno, registry, services, Endpoint, Error as MsgError, Message, Parcel,
};
use user::sys;

use super::bt::BtFn;
use super::bus::Controller;

/// Requests served per loop pass at most.
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

    /// Register or withdraw the name to match whether a controller is bound.
    pub(super) fn sync(&mut self, controllers: &mut [Controller]) {
        let bound = controllers.iter_mut().any(|c| c.bt().is_some());
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
                        trace::line!(trace::Channel::Usbd, "BT:SERVE name={}", api::NAME);
                        self.ends = Some((published, server));
                    }
                    Err(error) => {
                        trace::line!(trace::Channel::Usbd, "BT:FAIL register {}", error.message());
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
                trace::line!(trace::Channel::Usbd, "BT:WITHDRAW name={}", api::NAME);
            }
            _ => {}
        }
    }

    /// Answer the queued requests, then send what `btd` queued and collect
    /// what the controller sent; whether there was anything to do.
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
            let target = controllers.iter_mut().find_map(Controller::bt);
            let reply = match target {
                Some((_, _, bt)) => dispatch(bt, &message),
                None => Err(err(errno::ENODEV)),
            };
            // A one-way `Kick` has no reply to carry a refusal: dropping it
            // is the refusal.
            let Some(txn) = message.txn else {
                continue;
            };
            let reply = reply.unwrap_or_else(|error| {
                services::error_reply(message.interface_id(), message.method(), error)
            });
            let _ = server.reply_or_drop(txn, &reply);
        }
        if let Some((hc, device, bt)) = controllers.iter_mut().find_map(Controller::bt) {
            served |= bt.serve(hc, device);
        }
        served
    }
}

fn dispatch(bt: &mut BtFn, message: &Message) -> Result<Parcel> {
    if message.interface_id() != api::INTERFACE {
        return Err(err(errno::EINVAL));
    }
    if !btpolicy::may_use_transport(message.caller.uid) {
        return Err(err(errno::EACCES));
    }
    let body = &message.parcel.body;
    let method = message.method();
    let reply = match method {
        wire::METHOD_DESCRIBE => wire::encode_describe_reply(&wire::DescribeReply {
            device: bt.describe(),
        })
        .map_err(MsgError::Parcel)?,
        wire::METHOD_STATS => wire::encode_stats_reply(&wire::StatsReply { stats: bt.stats() })
            .map_err(MsgError::Parcel)?,
        wire::METHOD_ATTACHRING => {
            // The decoded request owns the ring buffer and the notify
            // endpoint; every way out but success closes them.
            let args = message.decode(wire::decode_attach_ring_args)?;
            let notify = Endpoint::from_raw(args.notify);
            match bt.attach(message.sender, &args, notify) {
                Ok(ring) => wire::encode_attach_ring_reply(&wire::AttachRingReply { ring })
                    .map_err(MsgError::Parcel)?,
                Err(error) => {
                    let _ = sys::buffer_close(args.rings.handle);
                    let _ = notify.release();
                    return Err(error);
                }
            }
        }
        wire::METHOD_DETACHRING => {
            let args = wire::decode_detach_ring_args(body).map_err(MsgError::Parcel)?;
            let owned = bt
                .sink
                .link
                .as_ref()
                .is_some_and(|link| link.owner == message.sender && link.ring == args.ring);
            if !owned {
                return Err(err(errno::EINVAL));
            }
            bt.sink.drop_link("detached by its owner");
            Vec::new()
        }
        // The frames move in `BtFn::serve`, which runs after this request:
        // the doorbell only has to wake the loop.
        wire::METHOD_KICK => Vec::new(),
        _ => return Err(err(errno::EINVAL)),
    };
    Ok(api::parcel(method, reply, Vec::new()))
}
