//! The loopback stack under `Net`: which destinations are this machine, where
//! a socket's replicas go, and polling `lo` until what it sent itself has
//! arrived.
//!
//! `lo` is a [`Stack`] like a card's, so a loopback connection is real TCP
//! (windows, FIN, RST: a `Connect` to a port nobody listens on is refused at
//! once) and the layers above, `netd` and the kernel's `AF_INET` sockets, see
//! nothing new. It answers for 127.0.0.1 and for every card's address, kept
//! in step whenever `lo` is used.

use alloc::vec::Vec;

use crate::stack::{SockError, Stack, LOOPBACK_ADDR, MAX_LOCAL_ADDRS};

use super::table::Bound;
use super::{Net, LOOPBACK, MAX_INTERFACES};

/// Rounds of `lo` per [`Net::poll`]: a connection takes about six (ARP both
/// ways, then the handshake), each round bounded by the stack's budgets.
const LOOPBACK_ROUNDS: usize = 16;

/// The card addresses `lo` holds, without allocating to compare them.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct LocalAddrs {
    addrs: [[u8; 4]; MAX_INTERFACES],
    count: usize,
}

impl LocalAddrs {
    fn as_slice(&self) -> &[[u8; 4]] {
        &self.addrs[..self.count]
    }
}

/// Where one replica of a socket goes: a slot, and on `lo` the one address it
/// answers on (`None`: any).
pub(super) type Placement = (usize, Option<[u8; 4]>);

impl Net {
    /// The loopback stack, for the tests and the statistics.
    pub fn loopback(&self) -> &Stack {
        &self.lo
    }

    /// Whether `dst` is this machine: the loopback address or a card's own.
    pub(super) fn is_local(&self, dst: [u8; 4]) -> bool {
        dst == LOOPBACK_ADDR || self.card_with_addr(dst).is_some()
    }

    /// The stack `lo`, holding the cards' current addresses.
    pub(super) fn lo_mut(&mut self) -> &mut Stack {
        let mut now = LocalAddrs::default();
        for (_, unit) in self.units() {
            let Some(addr) = unit.stack.state().addr else {
                continue;
            };
            // `lo` holds 127.0.0.1 and at most this many more.
            if now.count < MAX_LOCAL_ADDRS - 1 {
                now.addrs[now.count] = addr;
                now.count += 1;
            }
        }
        if now != self.lo_addrs {
            self.lo.set_local_addrs(now.as_slice());
            self.lo_addrs = now;
        }
        &mut self.lo
    }

    /// Run `lo` until it has received everything it sent (or the round
    /// limit: the rest waits for the next poll, which `poll_delay_ms` asks
    /// for at once).
    pub(super) fn poll_loopback(&mut self, now_ms: i64) {
        let lo = self.lo_mut();
        for _ in 0..LOOPBACK_ROUNDS {
            lo.poll(now_ms);
            if lo.looped_frames() == 0 {
                break;
            }
        }
    }

    /// When `lo` next needs a poll: now if frames are still in flight.
    pub(super) fn loopback_delay_ms(&mut self, now_ms: i64) -> Option<u64> {
        if self.lo.looped_frames() > 0 {
            return Some(0);
        }
        self.lo.poll_delay_ms(now_ms)
    }

    /// The interface that carries a connection or datagram to `peer` from a
    /// socket bound as `bound`: `lo` for this machine, else the interface it
    /// was bound to (if usable) or the route.
    pub(super) fn carrier(&self, peer: [u8; 4], bound: Option<Bound>) -> Result<usize, SockError> {
        if self.is_local(peer) {
            return Ok(LOOPBACK);
        }
        match bound.and_then(|b| b.only) {
            Some(LOOPBACK) => Err(SockError::Unreachable),
            Some(slot) => self
                .unit(slot)
                .filter(|unit| unit.usable())
                .map(|_| slot)
                .ok_or(SockError::Unreachable),
            None => self.route_for(peer).ok_or(SockError::Unreachable),
        }
    }

    /// Where a listener or a bound datagram socket needs replicas: every
    /// card and `lo` when bound to any address; the card and `lo` (on that
    /// address alone) when bound to a card's address; `lo` alone when bound
    /// to the loopback address.
    pub(super) fn placements(&self, bound: Option<Bound>) -> Vec<Placement> {
        let only = bound.and_then(|b| b.only);
        let addr = bound.map(|b| b.addr).filter(|a| *a != [0; 4]);
        let mut slots: Vec<Placement> = match only {
            None => self.units().map(|(slot, _)| (slot, None)).collect(),
            Some(LOOPBACK) => Vec::new(),
            Some(slot) => alloc::vec![(slot, None)],
        };
        slots.push((LOOPBACK, addr));
        slots
    }
}
