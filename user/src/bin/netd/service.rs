//! `os.lazy.net.stack.v1` (`idl/net.midl`): request dispatch for the stack.
//!
//! Nothing in a request names a caller: the kernel-stamped sender is the only
//! identity, used here for one thing, the per-caller cap on parked pings (the
//! ACL, not this code, decides who may call which method). A `Ping` is *parked*:
//! the transaction id is kept and the reply is sent when the stack reports the
//! result, so one thread serves any number of waiting callers. The listing
//! calls are in `lists.rs`; what is announced is in `publish.rs`.

use alloc::vec::Vec;

use libmessenger::server::{fault_reply, Caller, Fault};
use netstack::stack::{PingError, PingOutcome};
use netstack::{Net, NetPingResult};
use user::messenger::netsock as sock_api;
use user::messenger::netsock::wire as sock_wire;
use user::messenger::netstack::{self as api, wire};
use user::messenger::{errno, Endpoint, Error as MsgError, Message, Parcel};

use super::inet::Inet;
use super::owners::Tasks;
use super::parked::Parked as ParkedSock;
use super::ports::Ports;
use super::publish::Publisher;
use super::resolve::ParkedLookup;
use super::sock::SockStats;

pub(super) type Result<T> = core::result::Result<T, MsgError>;

/// Pings one caller may have waiting at once.
const PER_CALLER_PINGS: usize = 4;
/// `ENETUNREACH`: no address or no route yet.
pub(super) const ENETUNREACH: i64 = 101;

pub(super) fn err(code: i64) -> MsgError {
    MsgError::Errno(-code)
}

struct Parked {
    seq: u16,
    txn: u64,
    sender: u64,
}

/// The request being answered, set by [`Netd::dispatch`].
#[derive(Default)]
pub(super) struct Call {
    /// Its transaction, when it expects a reply.
    pub(super) txn: Option<u64>,
    pub(super) now_ms: i64,
    /// The socket owner a socket call resolved to (for the denial audit).
    pub(super) owner: u64,
    /// The reply is parked: `dispatch` answers nothing.
    pub(super) held: bool,
}

pub(super) struct Netd {
    /// Every interface and the socket table over them.
    pub(super) net: Net,
    /// The cards behind the interfaces.
    pub(super) ports: Ports,
    parked: Vec<Parked>,
    pub(super) lookups: Vec<ParkedLookup>,
    pub(super) parked_socks: Vec<ParkedSock>,
    pub(super) sock_stats: SockStats,
    pub(super) tasks: Tasks,
    /// The pump for Linux programs' sockets (stage N5).
    pub(super) inet: Inet,
    /// The request being answered (what a method that parks its call needs).
    pub(super) call: Call,
    /// Replies the event loop sends before it waits again.
    pub(super) outbox: Vec<(u64, Parcel)>,
    /// A `Reattach` call asked for every attachment to be rebuilt.
    pub(super) reattach: bool,
    publisher: Publisher,
}

impl Netd {
    pub(super) fn new(net: Net) -> Netd {
        Netd {
            net,
            ports: Ports::new(),
            parked: Vec::new(),
            lookups: Vec::new(),
            parked_socks: Vec::new(),
            sock_stats: SockStats::default(),
            tasks: Tasks::new(),
            inet: Inet::new(),
            call: Call::default(),
            outbox: Vec::new(),
            reattach: false,
            publisher: Publisher::new(),
        }
    }

    /// Answer one request through the generated servers. `None` means the
    /// call was parked (a method that parks calls [`Netd::hold`]): its reply
    /// is sent later, from the event loop.
    pub(super) fn dispatch(&mut self, message: &Message, now_ms: i64) -> Option<Parcel> {
        self.call = Call {
            txn: message.txn,
            now_ms,
            ..Call::default()
        };
        let reply = if message.interface_id() == sock_api::INTERFACE {
            sock_wire::dispatch(self, message)
        } else {
            wire::dispatch(self, message)
        };
        if self.sock_stats.owner_denied.replace(false) {
            self.note_denied(self.call.owner, message.method());
        }
        (!self.call.held).then_some(reply)
    }

    /// Keep the current call's transaction: the reply is sent when the stack
    /// has the result. The returned fault is what the generated server frames
    /// and `dispatch` then drops.
    pub(super) fn hold(&mut self) -> Fault {
        self.call.held = true;
        Fault::unserved()
    }

    /// Start a ping and park the call. Every argument is validated before
    /// anything is allocated or sent.
    fn start_ping(
        &mut self,
        sender: u64,
        dst: &[u8],
        payload_len: u32,
        timeout_ms: u32,
    ) -> Result<()> {
        // A reply needs a transaction; a one-way ping has nobody to answer.
        let Some(txn) = self.call.txn else {
            return Err(err(errno::EINVAL));
        };
        let dst: [u8; 4] = dst.try_into().map_err(|_| err(errno::EINVAL))?;
        if payload_len > netstack::stack::MAX_PING_PAYLOAD as u32
            || !(10..=60_000).contains(&timeout_ms)
        {
            return Err(err(errno::EINVAL));
        }
        if self.parked.iter().filter(|p| p.sender == sender).count() >= PER_CALLER_PINGS {
            return Err(err(errno::EAGAIN));
        }
        let seq = self
            .net
            .ping(
                dst,
                payload_len as usize,
                u64::from(timeout_ms),
                self.call.now_ms,
            )
            .map_err(|error| match error {
                PingError::BadArgument => err(errno::EINVAL),
                PingError::NoAddress | PingError::NoRoute => err(ENETUNREACH),
                PingError::Busy => err(errno::EAGAIN),
            })?;
        self.parked.push(Parked { seq, txn, sender });
        Ok(())
    }

    /// Answer the parked pings the stack has finished with.
    pub(super) fn finish_pings(&mut self, server: &Endpoint) {
        for NetPingResult { seq, outcome } in self.net.take_ping_results() {
            let Some(at) = self.parked.iter().position(|p| p.seq == seq) else {
                continue;
            };
            let parked = self.parked.remove(at);
            let reply = match outcome {
                PingOutcome::Reply {
                    rtt_ms,
                    source,
                    bytes,
                } => wire::encode_ping_reply(&wire::PingReply {
                    result: wire::EchoResult {
                        rtt_ms,
                        source: source.to_vec(),
                        bytes,
                    },
                })
                .map(|body| api::parcel(wire::METHOD_PING, body))
                .map_err(MsgError::Parcel),
                PingOutcome::TimedOut => Err(err(errno::ETIMEDOUT)),
            };
            let reply = reply.unwrap_or_else(|error| {
                fault_reply(api::INTERFACE, wire::METHOD_PING, &Fault::from(error))
            });
            // A caller that gave up or died has no transaction left: not a fault.
            let _ = server.reply_or_drop(parked.txn, &reply);
        }
    }

    /// Publish what changed: addresses, the interface list, the up event.
    pub(super) fn publish_if_changed(&mut self) {
        self.publisher.sync(&self.net, &self.ports);
    }
}

impl wire::Server for Netd {
    fn interfaces(&mut self, _: &Caller) -> core::result::Result<Vec<wire::InterfaceInfo>, Fault> {
        Ok(super::lists::interface_infos(&self.net, &self.ports))
    }

    fn addresses(&mut self, _: &Caller) -> core::result::Result<Vec<wire::AddressInfo>, Fault> {
        Ok(self.address_list())
    }

    fn routes(&mut self, _: &Caller) -> core::result::Result<Vec<wire::RouteInfo>, Fault> {
        Ok(self.route_list())
    }

    fn stats(&mut self, _: &Caller) -> core::result::Result<wire::StackStats, Fault> {
        Ok(self.stack_stats())
    }

    fn ping(
        &mut self,
        cx: &Caller,
        dst: Vec<u8>,
        payload_len: u32,
        timeout_ms: u32,
    ) -> core::result::Result<wire::EchoResult, Fault> {
        self.start_ping(cx.sender, &dst, payload_len, timeout_ms)?;
        Err(self.hold())
    }

    fn resolve(
        &mut self,
        cx: &Caller,
        name: alloc::string::String,
        timeout_ms: u32,
    ) -> core::result::Result<Vec<Vec<u8>>, Fault> {
        self.start_lookup(cx.sender, &name, timeout_ms)?;
        Err(self.hold())
    }

    fn renew(&mut self, _: &Caller) -> core::result::Result<(), Fault> {
        self.net.renew();
        Ok(())
    }

    fn reattach(&mut self, _: &Caller) -> core::result::Result<(), Fault> {
        self.reattach = true;
        Ok(())
    }
}
