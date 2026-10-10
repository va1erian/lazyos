//! Pings and name lookups across interfaces: each runs on the interface
//! [`Net`] picks (a ping by destination, a lookup by the resolvers' owner) and
//! is known to the caller by a `Net`-wide token, since every interface counts
//! its own.

use alloc::vec::Vec;

use crate::config::{is_usable_unicast, parse_ipv4};
use crate::stack::{
    valid_host_name, LookupOutcome, PingError, PingOutcome, ResolveError, MAX_LOOKUPS, MAX_PINGS,
    MAX_PING_PAYLOAD,
};

use super::Net;

/// The end of one ping, by `Net` token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetPingResult {
    pub seq: u16,
    pub outcome: PingOutcome,
}

/// The end of one lookup, by `Net` token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetLookupResult {
    pub id: u32,
    pub outcome: LookupOutcome,
}

/// One outstanding probe: its `Net` token, the interface it runs on and that
/// interface's own token.
#[derive(Clone, Copy)]
struct Mapped<T> {
    token: T,
    unit: usize,
    local: T,
}

pub(super) struct Probes {
    pings: Vec<Mapped<u16>>,
    lookups: Vec<Mapped<u32>>,
    next_ping: u16,
    next_lookup: u32,
    /// Results of probes whose interface was removed.
    orphaned_pings: Vec<NetPingResult>,
    orphaned_lookups: Vec<NetLookupResult>,
}

impl Probes {
    pub fn new() -> Probes {
        Probes {
            pings: Vec::new(),
            lookups: Vec::new(),
            next_ping: 1,
            next_lookup: 1,
            orphaned_pings: Vec::new(),
            orphaned_lookups: Vec::new(),
        }
    }

    /// The interface `unit` is gone: its probes end as timed out.
    pub fn drop_unit(&mut self, unit: usize) {
        for gone in self.pings.iter().filter(|m| m.unit == unit) {
            self.orphaned_pings.push(NetPingResult {
                seq: gone.token,
                outcome: PingOutcome::TimedOut,
            });
        }
        self.pings.retain(|m| m.unit != unit);
        for gone in self.lookups.iter().filter(|m| m.unit == unit) {
            self.orphaned_lookups.push(NetLookupResult {
                id: gone.token,
                outcome: LookupOutcome::TimedOut,
            });
        }
        self.lookups.retain(|m| m.unit != unit);
    }

    fn ping_token(&mut self) -> u16 {
        loop {
            let token = self.next_ping;
            self.next_ping = self.next_ping.wrapping_add(1).max(1);
            if self.pings.iter().all(|m| m.token != token) {
                return token;
            }
        }
    }

    fn lookup_token(&mut self) -> u32 {
        loop {
            let token = self.next_lookup;
            self.next_lookup = self.next_lookup.wrapping_add(1).max(1);
            if self.lookups.iter().all(|m| m.token != token) {
                return token;
            }
        }
    }
}

impl Net {
    pub(super) fn forget_probes(&mut self, unit: usize) {
        self.probes.drop_unit(unit);
    }

    /// Start one echo request to `dst` on the interface that routes it;
    /// returns the token its result carries.
    pub fn ping(
        &mut self,
        dst: [u8; 4],
        payload_len: usize,
        timeout_ms: u64,
        now_ms: i64,
    ) -> Result<u16, PingError> {
        if payload_len > MAX_PING_PAYLOAD || !is_usable_unicast(dst) || dst == [255; 4] {
            return Err(PingError::BadArgument);
        }
        if self.probes.pings.len() >= MAX_PINGS {
            return Err(PingError::Busy);
        }
        let Some(unit) = self.route_for(dst) else {
            let any_address = self.units().any(|(_, u)| u.usable());
            return Err(if any_address {
                PingError::NoRoute
            } else {
                PingError::NoAddress
            });
        };
        let stack = &mut self.unit_mut(unit).expect("routed").stack;
        let local = stack.ping(dst, payload_len, timeout_ms, now_ms)?;
        let token = self.probes.ping_token();
        self.probes.pings.push(Mapped { token, unit, local });
        Ok(token)
    }

    /// Give up on a ping (its caller went away).
    pub fn cancel_ping(&mut self, seq: u16) {
        let Some(at) = self.probes.pings.iter().position(|m| m.token == seq) else {
            return;
        };
        let gone = self.probes.pings.remove(at);
        if let Some(unit) = self.unit_mut(gone.unit) {
            unit.stack.cancel_ping(gone.local);
        }
    }

    /// Finished pings since the last call.
    pub fn take_ping_results(&mut self) -> Vec<NetPingResult> {
        let mut results = core::mem::take(&mut self.probes.orphaned_pings);
        let slots: Vec<usize> = self.units().map(|(slot, _)| slot).collect();
        for slot in slots {
            let done = self
                .unit_mut(slot)
                .expect("listed")
                .stack
                .take_ping_results();
            for result in done {
                let Some(at) = self
                    .probes
                    .pings
                    .iter()
                    .position(|m| m.unit == slot && m.local == result.seq)
                else {
                    continue;
                };
                let mapped = self.probes.pings.remove(at);
                results.push(NetPingResult {
                    seq: mapped.token,
                    outcome: result.outcome,
                });
            }
        }
        results
    }

    /// Pings still waiting.
    pub fn pings_outstanding(&self) -> usize {
        self.probes.pings.len()
    }

    /// Start a lookup of `name` at the resolvers of [`Net::dns_servers`]; the
    /// answer arrives through [`Net::take_lookup_results`]. A literal address
    /// is answered at once by any interface that has an address.
    pub fn resolve(
        &mut self,
        name: &str,
        timeout_ms: u64,
        now_ms: i64,
    ) -> Result<u32, ResolveError> {
        if !valid_host_name(name) {
            return Err(ResolveError::BadName);
        }
        if self.probes.lookups.len() >= MAX_LOOKUPS {
            return Err(ResolveError::Busy);
        }
        let unit = match self.dns_slot() {
            Some(slot) => slot,
            None if parse_ipv4(name).is_some() => self
                .units()
                .find(|(_, u)| u.usable())
                .map(|(slot, _)| slot)
                .ok_or(ResolveError::NoResolver)?,
            None => return Err(ResolveError::NoResolver),
        };
        let stack = &mut self.unit_mut(unit).expect("chosen").stack;
        let local = stack.resolve(name, timeout_ms, now_ms)?;
        let token = self.probes.lookup_token();
        self.probes.lookups.push(Mapped { token, unit, local });
        Ok(token)
    }

    /// Lookups outstanding.
    pub fn lookups_outstanding(&self) -> usize {
        self.probes.lookups.len()
    }

    /// The lookups that finished since the last call.
    pub fn take_lookup_results(&mut self) -> Vec<NetLookupResult> {
        let mut results = core::mem::take(&mut self.probes.orphaned_lookups);
        let slots: Vec<usize> = self.units().map(|(slot, _)| slot).collect();
        for slot in slots {
            let done = self
                .unit_mut(slot)
                .expect("listed")
                .stack
                .take_lookup_results();
            for result in done {
                let Some(at) = self
                    .probes
                    .lookups
                    .iter()
                    .position(|m| m.unit == slot && m.local == result.id)
                else {
                    continue;
                };
                let mapped = self.probes.lookups.remove(at);
                results.push(NetLookupResult {
                    id: mapped.token,
                    outcome: result.outcome,
                });
            }
        }
        results
    }
}
