//! `Resolve` (`os.lazy.net.stack.v1`): name lookups, parked like pings.
//!
//! The call is validated before a query exists (`netstack::valid_host_name`,
//! the timeout, the per-caller and total limits); the stack asks the
//! resolver and reports the end of each lookup, which is turned into the
//! reply here.

use alloc::vec::Vec;

use libmessenger::server::{fault_reply, Fault};
use netstack::stack::{LookupOutcome, ResolveError};
use netstack::NetLookupResult;
use user::messenger::netstack::{self as api, wire};
use user::messenger::{errno, Endpoint, Error as MsgError};

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
    pub(super) fn start_lookup(&mut self, sender: u64, name: &str, timeout_ms: u32) -> Result<()> {
        let Some(txn) = self.call.txn else {
            return Err(err(errno::EINVAL));
        };
        if !(10..=60_000).contains(&timeout_ms) {
            return Err(err(errno::EINVAL));
        }
        let mine = self.lookups.iter().filter(|l| l.sender == sender).count();
        if mine >= PER_CALLER_LOOKUPS {
            return Err(err(errno::EAGAIN));
        }
        let id = self
            .net
            .resolve(name, u64::from(timeout_ms), self.call.now_ms)
            .map_err(|error| match error {
                ResolveError::BadName => err(errno::EINVAL),
                ResolveError::NoResolver => err(ENETUNREACH),
                ResolveError::Busy => err(errno::EAGAIN),
            })?;
        self.lookups.push(ParkedLookup { id, txn, sender });
        Ok(())
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
                fault_reply(api::INTERFACE, wire::METHOD_RESOLVE, &Fault::from(error))
            });
            let _ = server.reply_or_drop(parked.txn, &reply);
        }
    }
}
