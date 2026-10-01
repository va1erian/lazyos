//! Name lookups (`Resolve`): A records through smoltcp's DNS socket, at the
//! resolvers DHCP or the static configuration gave.
//!
//! A dotted quad needs no query and is answered at once. Everything else is
//! validated before a query exists (length, labels, characters), at most
//! [`MAX_LOOKUPS`] run at once, and each has a deadline of its own, so a
//! resolver that never answers cannot hold a slot.

use alloc::vec;
use alloc::vec::Vec;

use smoltcp::socket::dns::{self, GetQueryResultError, QueryHandle};
use smoltcp::wire::{DnsQueryType, IpAddress};

use super::*;
use crate::config::parse_ipv4;

/// Lookups outstanding at once.
pub const MAX_LOOKUPS: usize = 8;
/// Longest host name, in bytes.
pub const MAX_NAME: usize = 253;

/// Why a lookup could not be started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolveError {
    /// The name is not a host name.
    BadName,
    /// No address yet, or no resolver to ask.
    NoResolver,
    /// [`MAX_LOOKUPS`] are already outstanding.
    Busy,
}

/// How a lookup ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LookupOutcome {
    /// One to four IPv4 addresses.
    Found(Vec<[u8; 4]>),
    /// The resolver answered and has no address for the name.
    NoSuchName,
    /// Nothing came back in time.
    TimedOut,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LookupResult {
    /// The token `resolve` returned.
    pub id: u32,
    pub outcome: LookupOutcome,
}

pub(super) struct Lookup {
    id: u32,
    handle: QueryHandle,
    deadline_ms: i64,
}

/// The DNS socket with room for [`MAX_LOOKUPS`] queries.
pub(super) fn dns_socket() -> dns::Socket<'static> {
    dns::Socket::new(&[], (0..MAX_LOOKUPS).map(|_| None).collect::<Vec<_>>())
}

/// Letters, digits and hyphens in labels of 1 to 63 bytes (a label neither
/// starts nor ends with a hyphen; underscore is allowed too, service names use
/// it), no empty label, at most [`MAX_NAME`].
pub fn valid_host_name(name: &str) -> bool {
    let name = name.strip_suffix('.').unwrap_or(name);
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}

impl Stack {
    /// Start a lookup of `name`; the answer arrives through
    /// [`Stack::take_lookup_results`]. A literal address is answered at once.
    pub fn resolve(
        &mut self,
        name: &str,
        timeout_ms: u64,
        now_ms: i64,
    ) -> Result<u32, ResolveError> {
        if !valid_host_name(name) {
            return Err(ResolveError::BadName);
        }
        if self.state.addr.is_none() {
            return Err(ResolveError::NoResolver);
        }
        let id = self.next_lookup;
        self.next_lookup = self.next_lookup.wrapping_add(1).max(1);
        if let Some(addr) = parse_ipv4(name) {
            self.lookup_results.push(LookupResult {
                id,
                outcome: LookupOutcome::Found(vec![addr]),
            });
            return Ok(id);
        }
        if self.state.dns.is_empty() {
            return Err(ResolveError::NoResolver);
        }
        if self.lookups.len() >= MAX_LOOKUPS {
            return Err(ResolveError::Busy);
        }
        let socket = self.sockets.get_mut::<dns::Socket>(self.dns);
        let handle = socket
            .start_query(self.iface.context(), name, DnsQueryType::A)
            .map_err(|error| match error {
                dns::StartQueryError::NoFreeSlot => ResolveError::Busy,
                _ => ResolveError::BadName,
            })?;
        self.lookups.push(Lookup {
            id,
            handle,
            deadline_ms: now_ms + timeout_ms as i64,
        });
        self.counters.lookups_sent += 1;
        Ok(id)
    }

    /// Lookups outstanding.
    pub fn lookups_outstanding(&self) -> usize {
        self.lookups.len()
    }

    /// The lookups that finished since the last call.
    pub fn take_lookup_results(&mut self) -> Vec<LookupResult> {
        core::mem::take(&mut self.lookup_results)
    }

    /// Point the DNS socket at the current resolvers.
    pub(super) fn sync_resolvers(&mut self) {
        let servers: Vec<IpAddress> = self
            .state
            .dns
            .iter()
            .map(|d| IpAddress::v4(d[0], d[1], d[2], d[3]))
            .collect();
        self.sockets
            .get_mut::<dns::Socket>(self.dns)
            .update_servers(&servers);
    }

    /// Collect finished queries and expire the ones past their deadline.
    pub(super) fn handle_dns(&mut self, now_ms: i64) {
        let mut i = 0;
        while i < self.lookups.len() {
            let Lookup {
                id,
                handle,
                deadline_ms,
            } = self.lookups[i];
            let socket = self.sockets.get_mut::<dns::Socket>(self.dns);
            let outcome = match socket.get_query_result(handle) {
                Ok(addrs) => {
                    let found: Vec<[u8; 4]> = addrs
                        .iter()
                        .map(|a| {
                            let IpAddress::Ipv4(v4) = a;
                            v4.octets()
                        })
                        .collect();
                    Some(if found.is_empty() {
                        LookupOutcome::NoSuchName
                    } else {
                        LookupOutcome::Found(found)
                    })
                }
                Err(GetQueryResultError::Failed) => Some(LookupOutcome::NoSuchName),
                Err(GetQueryResultError::Pending) if now_ms >= deadline_ms => {
                    socket.cancel_query(handle);
                    Some(LookupOutcome::TimedOut)
                }
                Err(GetQueryResultError::Pending) => None,
            };
            let Some(outcome) = outcome else {
                i += 1;
                continue;
            };
            match outcome {
                LookupOutcome::Found(_) => self.counters.lookups_answered += 1,
                _ => self.counters.lookups_failed += 1,
            }
            self.lookups.remove(i);
            self.lookup_results.push(LookupResult { id, outcome });
        }
    }

    /// Milliseconds until the nearest lookup deadline.
    pub(super) fn lookup_delay_ms(&self, now_ms: i64) -> Option<u64> {
        self.lookups
            .iter()
            .map(|l| (l.deadline_ms - now_ms).max(0) as u64)
            .min()
    }
}
