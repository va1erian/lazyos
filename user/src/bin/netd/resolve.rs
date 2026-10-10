//! `Resolve` (`os.lazy.net.stack.v1`): name lookups, parked like pings.
//!
//! The call is validated before a query exists (`netstack::valid_host_name`,
//! the timeout, the per-caller and total limits); the stack asks the
//! resolver and reports the end of each lookup, which is turned into the
//! reply here.

use alloc::vec::Vec;

use netstack::stack::{LookupOutcome, ResolveError};
use netstack::NetLookupResult;
use user::messenger::netstack::{self as api, wire};
use user::messenger::{errno, services, Endpoint, Error as MsgError, Message, Parcel};

use super::service::Netd;

type Result<T> = core::result::Result<T, MsgError>;

/// Lookups one caller may have waiting at once.
const PER_CALLER_LOOKUPS: usize = 4;
/// `ENETUNREACH`: no address, or no resolver.
const ENETUNREACH: i64 = 101;

fn err(code: i64) -> MsgError {
    MsgError::Errno(-code)
}

pub(super) struct ParkedLookup {
    id: u32,
    txn: u64,
    sender: u64,
}

impl Netd {
    /// Start a lookup and park the call.
    pub(super) fn resolve(&mut self, message: &Message, now_ms: i64) -> Result<Option<Parcel>> {
        let args = wire::decode_resolve_args(&message.parcel.body).map_err(MsgError::Parcel)?;
        let Some(txn) = message.txn else {
            return Err(err(errno::EINVAL));
        };
        if !(10..=60_000).contains(&args.timeout_ms) {
            return Err(err(errno::EINVAL));
        }
        let mine = self
            .lookups
            .iter()
            .filter(|l| l.sender == message.sender)
            .count();
        if mine >= PER_CALLER_LOOKUPS {
            return Err(err(errno::EAGAIN));
        }
        let id = self
            .net
            .resolve(&args.name, u64::from(args.timeout_ms), now_ms)
            .map_err(|error| match error {
                ResolveError::BadName => err(errno::EINVAL),
                ResolveError::NoResolver => err(ENETUNREACH),
                ResolveError::Busy => err(errno::EAGAIN),
            })?;
        self.lookups.push(ParkedLookup {
            id,
            txn,
            sender: message.sender,
        });
        Ok(None)
    }

    /// Answer the lookups the stack has finished with.
    pub(super) fn finish_lookups(&mut self, server: &Endpoint) {
        for NetLookupResult { id, outcome } in self.net.take_lookup_results() {
            let Some(at) = self.lookups.iter().position(|l| l.id == id) else {
                continue;
            };
            let parked = self.lookups.remove(at);
            let reply = match outcome {
                LookupOutcome::Found(addrs) => wire::encode_resolve_reply(&wire::ResolveReply {
                    addrs: addrs.iter().map(|a| a.to_vec()).collect::<Vec<_>>(),
                })
                .map(|body| api::parcel(wire::METHOD_RESOLVE, body))
                .map_err(MsgError::Parcel),
                LookupOutcome::NoSuchName => Err(err(errno::ENOENT)),
                LookupOutcome::TimedOut => Err(err(errno::ETIMEDOUT)),
            };
            let reply = reply.unwrap_or_else(|error| {
                services::error_reply(api::INTERFACE, wire::METHOD_RESOLVE, error)
            });
            let _ = server.reply_or_drop(parked.txn, &reply);
        }
    }
}
