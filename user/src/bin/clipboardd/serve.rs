//! Service entry point: the serve loop, credential lookup and request
//! dispatch.

use alloc::string::String;
use alloc::vec::Vec;
use libmessenger::server::{Caller, Fault};
use libmessenger::Buffer;
use user::messenger::clipboard::wire as proto;
use user::messenger::{self, clipboard as wire, errno, registry, wait, Error};
use user::sys;
use user::trace;

use super::args::{demo_from_args, history_from_args, spawn_demo};
use super::state::Clipboard;

/// Register the service and serve offers, pastes and the changed topic.
pub(super) fn run() -> messenger::Result<()> {
    let history = history_from_args();
    let (published, server) = messenger::create_pair()?;
    registry::register(wire::NAME, &published, &[wire::INTERFACE], 0)?;
    let mut clipboard = Clipboard::new(history);
    trace::line!(trace::Channel::Clipboard, "HISTORY:{history}");
    trace::line!(trace::Channel::Clipboard, "READY");
    // Serving: what waits for this service may start (init.Ready, P7.3).
    user::messenger::services::init::notify_ready();
    // The demo pair (if requested) starts right here, so its two ELF loads
    // complete before the supervisor's crash test files its short-deadline
    // health report; the evidence then runs while the rest of boot is settled.
    let mut demo_pending = demo_from_args();
    let mut demo_children = 0u64;
    // One receive buffer for the whole life of the service: the user bump
    // allocator never reclaims per-call buffers, so long-lived loops must not
    // allocate one per request.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    loop {
        if demo_pending {
            demo_children = spawn_demo();
            demo_pending = false;
        }
        // Park until a request or, while a demo child is alive, its exit
        // (the child bell, P7); never on a timer.
        let doorbells = if demo_children > 0 {
            wait::WAIT_CHILD
        } else {
            0
        };
        let ready = wait::wait_any(&[server], doorbells, None)?;
        if ready & wait::CHILD_READY != 0 && sys::wait(sys::clock().max(1)).is_some() {
            demo_children -= 1;
        }
        if ready & 1 == 0 {
            continue;
        }
        match server.recv_with(&mut buffer, Some(messenger::EXPIRED_DEADLINE)) {
            Ok(message) => {
                let reply = proto::dispatch(&mut Handler(&mut clipboard), &message);
                if let Some(txn) = message.txn {
                    // A caller whose deadline passed is a normal scheduling
                    // race: the kernel expired the transaction and the reply
                    // is `-ENOENT`. A caller whose channel closed before the
                    // reply was written (`-EPIPE`, e.g. a short-lived probe or
                    // an offer client that has already dropped its endpoint)
                    // is equally normal. Keep serving the other sessions.
                    if let Err(error) = server.reply(txn, &reply) {
                        match error.errno() {
                            Some(code) if code == -errno::ENOENT || code == -errno::EPIPE => {}
                            _ => return Err(error),
                        }
                    }
                }
            }
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => {}
            Err(error) => return Err(error),
        }
    }
}

/// The service state, answering `os.lazy.clipboard.v1` (the generated server).
/// The kernel-stamped actor is the caller: its session scopes every offer, and
/// its slot names its endpoint in the owner cache (issue #446).
struct Handler<'a>(&'a mut Clipboard);

fn actor(cx: &Caller) -> sys::Cred {
    sys::Cred::new(cx.uid, cx.gid, cx.caps, cx.label_id, cx.session)
}

impl proto::Server for Handler<'_> {
    fn offer(
        &mut self,
        cx: &Caller,
        owner: String,
        sink: Option<String>,
        mimes: Vec<String>,
        data: Vec<proto::Payload>,
    ) -> Result<u64, Fault> {
        let request = wire::OfferRequest {
            owner,
            sink,
            mimes,
            data: data
                .into_iter()
                .map(|payload| (payload.mime, payload.bytes))
                .collect(),
        };
        Ok(self.0.offer(request, cx.session, cx.sender, sys::clock())?)
    }

    fn offer_buffer(
        &mut self,
        cx: &Caller,
        owner: String,
        mime: String,
        data: Buffer,
    ) -> Result<u64, Fault> {
        // The buffer-carrying methods travel on the interface's own id: the
        // kernel's declared-object gate keys on the header's interface (an
        // app's manifest allows it along with the two scopes).
        let request = proto::OfferBufferArgs { owner, mime, data };
        Ok(self
            .0
            .offer_buffer(request, cx.session, cx.sender, sys::clock())?)
    }

    fn request(
        &mut self,
        cx: &Caller,
        token: u64,
        mime: String,
    ) -> Result<proto::RequestReply, Fault> {
        let cred = actor(cx);
        let handle = self.0.request(token, &mime, cred.session);
        match &handle {
            Ok(handle) => self.0.log_paste(&cred, cx.sender, handle),
            Err(Error::Errno(code)) if *code == -errno::EACCES => {
                self.0.log_denial(&cred, cx.sender, &mime, token);
            }
            Err(_) => {}
        }
        let handle = handle?;
        Ok(proto::RequestReply {
            token: handle.token,
            mime: handle.mime,
            lazy: handle.lazy,
            bytes: handle.bytes,
            size: handle.size,
        })
    }

    fn fetch(&mut self, cx: &Caller, token: u64, mime: String, into: Buffer) -> Result<u64, Fault> {
        let cred = actor(cx);
        let request = proto::FetchArgs {
            token,
            mime: mime.clone(),
            into,
        };
        let written = self.0.fetch(request, cred.session);
        if let Err(Error::Errno(code)) = &written {
            if *code == -errno::EACCES {
                self.0.log_denial(&cred, cx.sender, &mime, token);
            }
        }
        Ok(written?)
    }

    fn current(&mut self, cx: &Caller) -> Result<Option<proto::OfferMeta>, Fault> {
        Ok(self.0.current(cx.session).as_ref().map(wire::to_wire_meta))
    }

    fn ping(&mut self, _: &Caller) -> Result<(), Fault> {
        Ok(())
    }
}
