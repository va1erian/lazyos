//! `os.lazy.net.stack.v1` (`idl/net.midl`): request dispatch for the stack.
//!
//! Nothing in a request names a caller: the kernel-stamped sender is the only
//! identity, used here for one thing, the per-caller cap on parked pings (the
//! ACL, not this code, decides who may call which method). A `Ping` is *parked*:
//! the transaction id is kept and the reply is sent when the stack reports the
//! result, so one thread serves any number of waiting callers. The listing
//! calls are in `lists.rs`; what is announced is in `publish.rs`.

use alloc::vec::Vec;

use netstack::stack::{PingError, PingOutcome};
use netstack::{Net, NetPingResult};
use user::messenger::netsock as sock_api;
use user::messenger::netstack::{self as api, wire};
use user::messenger::{errno, services, Endpoint, Error as MsgError, Message, Parcel};

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
            outbox: Vec::new(),
            reattach: false,
            publisher: Publisher::new(),
        }
    }

    /// Route one request. `Ok(Some)` is the reply; `Ok(None)` means the call
    /// was parked and will be answered later; `Err` becomes the error reply.
    pub(super) fn dispatch(&mut self, message: &Message, now_ms: i64) -> Result<Option<Parcel>> {
        if message.interface_id() == sock_api::INTERFACE {
            return self.dispatch_socket(message, now_ms);
        }
        if message.interface_id() != api::INTERFACE {
            return Err(err(errno::EINVAL));
        }
        let method = message.method();
        let body = match method {
            wire::METHOD_INTERFACES => self.interfaces()?,
            wire::METHOD_ADDRESSES => self.addresses()?,
            wire::METHOD_ROUTES => self.routes()?,
            wire::METHOD_STATS => self.stats()?,
            wire::METHOD_PING => return self.ping(message, now_ms),
            wire::METHOD_RESOLVE => return self.resolve(message, now_ms),
            wire::METHOD_RENEW => {
                self.net.renew();
                Vec::new()
            }
            wire::METHOD_REATTACH => {
                self.reattach = true;
                Vec::new()
            }
            _ => return Err(err(errno::EINVAL)),
        };
        Ok(Some(api::parcel(method, body)))
    }

    /// Start a ping and park the call. Every argument is validated before
    /// anything is allocated or sent.
    fn ping(&mut self, message: &Message, now_ms: i64) -> Result<Option<Parcel>> {
        let args = wire::decode_ping_args(&message.parcel.body).map_err(MsgError::Parcel)?;
        // A reply needs a transaction; a one-way ping has nobody to answer.
        let Some(txn) = message.txn else {
            return Err(err(errno::EINVAL));
        };
        let dst: [u8; 4] = args
            .dst
            .as_slice()
            .try_into()
            .map_err(|_| err(errno::EINVAL))?;
        if args.payload_len > netstack::stack::MAX_PING_PAYLOAD as u32
            || !(10..=60_000).contains(&args.timeout_ms)
        {
            return Err(err(errno::EINVAL));
        }
        if self
            .parked
            .iter()
            .filter(|p| p.sender == message.sender)
            .count()
            >= PER_CALLER_PINGS
        {
            return Err(err(errno::EAGAIN));
        }
        let seq = self
            .net
            .ping(
                dst,
                args.payload_len as usize,
                u64::from(args.timeout_ms),
                now_ms,
            )
            .map_err(|error| match error {
                PingError::BadArgument => err(errno::EINVAL),
                PingError::NoAddress | PingError::NoRoute => err(ENETUNREACH),
                PingError::Busy => err(errno::EAGAIN),
            })?;
        self.parked.push(Parked {
            seq,
            txn,
            sender: message.sender,
        });
        Ok(None)
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
                services::error_reply(api::INTERFACE, wire::METHOD_PING, error)
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
