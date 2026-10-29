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

use crate::stack::{Kind, SockAddr, SockError, Stack, LOOPBACK_ADDR, MAX_LOCAL_ADDRS};

use super::table::{Bound, Replica, Shape, VSock};
use super::{Net, LOOPBACK, MAX_INTERFACES};

/// Rounds of `lo` per [`Net::poll`]: a handshake takes three, a burst of
/// data and its acknowledgements a few more; each round is bounded by the
/// stack's budgets.
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

    /// The card addresses `lo` answers for: those of the first cards that
    /// hold one, as many as fit beside 127.0.0.1.
    fn local_addrs(&self) -> LocalAddrs {
        let mut held = LocalAddrs::default();
        for (_, unit) in self.units() {
            if held.count == MAX_LOCAL_ADDRS - 1 {
                break;
            }
            if let Some(addr) = unit.stack.state().addr {
                held.addrs[held.count] = addr;
                held.count += 1;
            }
        }
        held
    }

    /// Whether `dst` is this machine: the loopback address or an address `lo`
    /// holds for a card.
    pub(super) fn is_local(&self, dst: [u8; 4]) -> bool {
        dst == LOOPBACK_ADDR || self.local_addrs().as_slice().contains(&dst)
    }

    /// The stack `lo`, holding the cards' current addresses.
    pub(super) fn lo_mut(&mut self) -> &mut Stack {
        let held = self.local_addrs();
        if held != self.lo_addrs {
            self.lo.set_local_addrs(held.as_slice());
            self.lo_addrs = held;
        }
        &mut self.lo
    }

    /// Run `lo` until it has received everything it sent (or the round
    /// limit: the rest waits for the next poll, which `poll_delay_ms` asks
    /// for at once).
    pub(super) fn poll_loopback(&mut self, now_ms: i64) {
        self.reset_departed();
        // An idle `lo` (nothing open, closing or in flight) has nothing to do.
        let lo = &self.lo;
        if lo.socket_open_count() == 0 && lo.socket_closing() == 0 && lo.looped_frames() == 0 {
            return;
        }
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

    /// Where a listener or a bound datagram socket needs replicas: the
    /// cards it serves, and for a datagram socket `lo` too (on its one
    /// address when bound to one). A listener gets its `lo` replica when a
    /// local connection first comes ([`Net::serve_locally`]): a replica holds
    /// a whole backlog of 256 KiB buffers, and most listeners never see one.
    /// A socket bound to 127.0.0.1 lives on `lo` alone.
    pub(super) fn placements(&self, bound: Bound, stream: bool) -> Vec<Placement> {
        let addr = (bound.addr != [0; 4]).then_some(bound.addr);
        if bound.only == Some(LOOPBACK) {
            return alloc::vec![(LOOPBACK, addr)];
        }
        let mut slots: Vec<Placement> = match bound.only {
            Some(slot) => alloc::vec![(slot, None)],
            None => self.units().map(|(slot, _)| (slot, None)).collect(),
        };
        if !stream && addr.is_none_or(|addr| self.is_local(addr)) {
            slots.push((LOOPBACK, addr));
        }
        slots
    }

    /// Before a connection to `to` over `lo`: give the listener on that port,
    /// if it serves `to`'s address and has no replica on `lo` yet, one there.
    /// A listener that cannot get one (its owner's quota) simply refuses.
    pub(super) fn serve_locally(&mut self, to: SockAddr) {
        let wanted = self.table.iter().find_map(|sock| {
            let (Shape::Spread { replicas, backlog }, Some(bound)) = (&sock.shape, sock.bound)
            else {
                return None;
            };
            let at = match bound.only {
                None => None,
                Some(slot) if slot != LOOPBACK && bound.addr == to.addr => Some(bound.addr),
                Some(_) => return None,
            };
            let serves = sock.kind == Kind::Stream && *backlog > 0 && bound.port == to.port;
            let placed = replicas.iter().any(|r| r.unit == LOOPBACK);
            (serves && !placed).then_some((sock.id, sock.owner, *backlog, at))
        });
        let Some((id, owner, backlog, at)) = wanted else {
            return;
        };
        let Ok(replica) = self.open_replica((LOOPBACK, at), owner, to.port, backlog) else {
            return;
        };
        if let Ok(VSock {
            shape: Shape::Spread { replicas, .. },
            ..
        }) = self.table.entry(id, owner)
        {
            replicas.push(replica);
        }
    }

    /// Reset the connections on `lo` that used a card address no card holds
    /// any longer (a lease that ended or changed): no packet to or from it can
    /// arrive, so they would otherwise wait for their timeout.
    fn reset_departed(&mut self) {
        let held = self.local_addrs();
        if held == self.lo_seen {
            return;
        }
        let departed = core::mem::replace(&mut self.lo_seen, held);
        for addr in departed.as_slice() {
            if !held.as_slice().contains(addr) {
                self.forget_on_lo(None, Some(*addr));
            }
        }
    }

    /// A card (`slot`, if it left) no longer holds `addr`: what `lo` held
    /// for it goes too. Sockets bound to that card lose their replicas on
    /// `lo`, and connections on `lo` that used the address are reset, as on
    /// the card itself.
    pub(super) fn forget_on_lo(&mut self, slot: Option<usize>, addr: Option<[u8; 4]>) {
        let Net { table, lo, .. } = self;
        for sock in table.iter_mut() {
            let bound_there = slot.is_some() && sock.bound.is_some_and(|bound| bound.only == slot);
            let on_lo: Vec<Replica> = match &sock.shape {
                Shape::Spread { replicas, .. } if bound_there => replicas.clone(),
                Shape::Pinned(replica) if replica.unit == LOOPBACK => alloc::vec![*replica],
                _ => continue,
            };
            for replica in on_lo.into_iter().filter(|r| r.unit == LOOPBACK) {
                let used =
                    |end: Result<SockAddr, SockError>| end.is_ok_and(|e| Some(e.addr) == addr);
                let gone = bound_there
                    || used(lo.socket_local_addr(replica.sid, sock.owner))
                    || used(lo.socket_peer_addr(replica.sid, sock.owner));
                if gone {
                    let _ = lo.socket_abort(replica.sid, sock.owner);
                    sock.shape = Shape::Lost;
                }
            }
        }
    }
}
