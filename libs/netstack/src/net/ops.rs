//! Socket operations that do not depend on the kind of socket: open, bind,
//! close, ownership, addresses, readiness, and the bookkeeping that keeps
//! replicas in step with the interfaces. Streams are in `ops_stream.rs`,
//! datagrams in `ops_dgram.rs`.

use alloc::vec::Vec;

use crate::stack::{ready, Kind, SockAddr, SockError, Stack, EPHEMERAL_FIRST, PRIVILEGED_PORTS};

use super::table::{Bound, Replica, Shape, VSock};
use super::{Net, Unit};

impl Net {
    /// Open sockets, all owners.
    pub fn socket_open_count(&self) -> usize {
        self.table.open_count()
    }

    /// Who owns at least one socket, each once.
    pub fn socket_owners(&self) -> Vec<u64> {
        self.table.owners()
    }

    /// Every open socket id, for the tests.
    pub fn socket_ids(&self) -> Vec<u32> {
        self.table.ids()
    }

    /// `Open`: a new socket of `kind` for `owner`. No interface is chosen yet.
    pub fn socket_open(&mut self, owner: u64, kind: Kind) -> Result<u32, SockError> {
        self.table.check_quota(owner)?;
        self.sockets.opened += 1;
        Ok(self.table.insert(owner, kind, Shape::Fresh))
    }

    /// `Bind`: choose the local port, and optionally an interface by its
    /// address (all zeros: any).
    pub fn socket_bind(&mut self, id: u32, owner: u64, local: SockAddr) -> Result<(), SockError> {
        let kind = self.table.entry(id, owner)?.kind;
        let only = if local.addr == [0; 4] {
            None
        } else {
            Some(
                self.slot_with_addr(local.addr)
                    .ok_or(SockError::BadAddress)?,
            )
        };
        let requested = self.check_port(kind, local.port)?;
        let entry = self.table.entry(id, owner)?;
        if entry.bound.is_some() || !matches!(entry.shape, Shape::Fresh) {
            return Err(SockError::InvalidState);
        }
        let port = match requested {
            Some(port) => port,
            None => self.pick_port(kind).ok_or(SockError::AddrInUse)?,
        };
        self.table.entry(id, owner)?.bound = Some(Bound { port, only });
        if kind == Kind::Datagram {
            if let Err(error) = self.datagram_spread(id, owner) {
                self.table.entry(id, owner)?.bound = None;
                return Err(error);
            }
        }
        Ok(())
    }

    /// `Close`.
    pub fn socket_close(&mut self, id: u32, owner: u64, now_ms: i64) -> Result<(), SockError> {
        let sock = self.table.take(id, owner)?;
        self.release(sock, now_ms);
        self.sockets.closed += 1;
        Ok(())
    }

    /// Reclaim everything `owner` holds (it exited); how many sockets.
    pub fn sockets_close_owner(&mut self, owner: u64, now_ms: i64) -> usize {
        let socks = self.table.take_owner(owner);
        let count = socks.len();
        for sock in socks {
            self.release(sock, now_ms);
        }
        self.sockets.reclaimed += count as u64;
        count
    }

    /// Give every interface socket behind `sock` back.
    fn release(&mut self, sock: VSock, now_ms: i64) {
        let replicas = match sock.shape {
            Shape::Pinned(replica) => alloc::vec![replica],
            Shape::Spread { replicas, .. } => replicas,
            Shape::Fresh | Shape::Lost => Vec::new(),
        };
        for replica in replicas {
            if let Some(unit) = self.unit_mut(replica.unit) {
                let _ = unit.stack.socket_close(replica.sid, sock.owner, now_ms);
            }
        }
    }

    /// Give socket `id` to `new_owner` (the kernel `AF_INET` pump learns a
    /// connection's final owner only after it has been accepted). Refused when
    /// `new_owner` is at its quota.
    pub fn socket_chown(&mut self, id: u32, owner: u64, new_owner: u64) -> Result<(), SockError> {
        if owner == new_owner {
            return self.table.entry(id, owner).map(|_| ());
        }
        self.table.entry(id, owner)?;
        self.table.check_quota(new_owner)?;
        let replicas = self.replicas_of(id, owner)?;
        for replica in replicas {
            self.stack(replica.unit)?
                .socket_chown(replica.sid, owner, new_owner)?;
        }
        self.table.entry(id, owner)?.owner = new_owner;
        Ok(())
    }

    /// `LocalAddr`: the socket's address and port.
    pub fn socket_local_addr(&mut self, id: u32, owner: u64) -> Result<SockAddr, SockError> {
        let entry = self.table.entry(id, owner)?;
        let (bound, shape) = (entry.bound, entry.shape.clone());
        match shape {
            Shape::Pinned(replica) => self
                .stack(replica.unit)?
                .socket_local_addr(replica.sid, owner),
            Shape::Spread { replicas, .. } => {
                for replica in replicas {
                    if let Ok(addr) = self
                        .stack(replica.unit)?
                        .socket_local_addr(replica.sid, owner)
                    {
                        return Ok(addr);
                    }
                }
                self.bound_addr(bound)
            }
            Shape::Fresh => self.bound_addr(bound),
            Shape::Lost => Err(SockError::Reset),
        }
    }

    /// The address a bound-but-not-placed socket reports: that of the
    /// interface it was bound to, else of the primary one.
    fn bound_addr(&self, bound: Option<Bound>) -> Result<SockAddr, SockError> {
        let bound = bound.ok_or(SockError::InvalidState)?;
        let slot = bound.only.or_else(|| self.primary());
        let addr = slot
            .and_then(|slot| self.unit(slot))
            .and_then(|unit| unit.stack.state().addr)
            .unwrap_or([0; 4]);
        Ok(SockAddr {
            addr,
            port: bound.port,
        })
    }

    /// `PeerAddr`: who the socket is connected to.
    pub fn socket_peer_addr(&mut self, id: u32, owner: u64) -> Result<SockAddr, SockError> {
        match self.table.entry(id, owner)?.shape.clone() {
            Shape::Pinned(replica) => self
                .stack(replica.unit)?
                .socket_peer_addr(replica.sid, owner),
            Shape::Lost => Err(SockError::Reset),
            Shape::Fresh | Shape::Spread { .. } => Err(SockError::NotConnected),
        }
    }

    /// `Poll`: the readiness bits of a socket right now.
    pub fn socket_readiness(&mut self, id: u32, owner: u64) -> Result<u32, SockError> {
        let entry = self.table.entry(id, owner)?;
        let (kind, shape) = (entry.kind, entry.shape.clone());
        match shape {
            Shape::Pinned(replica) => self
                .stack(replica.unit)?
                .socket_readiness(replica.sid, owner),
            Shape::Spread { replicas, .. } => {
                let mut bits = 0;
                for replica in replicas {
                    bits |= self
                        .stack(replica.unit)?
                        .socket_readiness(replica.sid, owner)?;
                }
                // A datagram socket can always queue a datagram, as its
                // replicas say when it has any.
                if kind == Kind::Datagram {
                    bits |= ready::WRITABLE;
                }
                Ok(bits)
            }
            Shape::Fresh if kind == Kind::Datagram => Ok(ready::WRITABLE),
            Shape::Fresh => Ok(0),
            Shape::Lost => Ok(ready::CLOSED | ready::READABLE),
        }
    }

    /// The interface sockets behind `id`.
    pub(super) fn replicas_of(&mut self, id: u32, owner: u64) -> Result<Vec<Replica>, SockError> {
        Ok(match &self.table.entry(id, owner)?.shape {
            Shape::Pinned(replica) => alloc::vec![*replica],
            Shape::Spread { replicas, .. } => replicas.clone(),
            Shape::Fresh | Shape::Lost => Vec::new(),
        })
    }

    /// The stack of interface `slot`; an interface that is gone is
    /// unreachable.
    pub(super) fn stack(&mut self, slot: usize) -> Result<&mut Stack, SockError> {
        self.unit_mut(slot)
            .map(|unit| &mut unit.stack)
            .ok_or(SockError::Unreachable)
    }

    /// Whether any socket, here or on an interface (a connection that picked
    /// its own ephemeral port), holds `port` for `kind`.
    fn port_taken(&self, kind: Kind, port: u16) -> bool {
        self.table.port_taken(kind, port)
            || self
                .units()
                .any(|(_, unit)| unit.stack.socket_port_in_use(kind, port))
    }

    /// Check a port a caller asked for: `Ok(None)` means "pick one".
    pub(super) fn check_port(&self, kind: Kind, port: u16) -> Result<Option<u16>, SockError> {
        if port == 0 {
            return Ok(None);
        }
        if port < PRIVILEGED_PORTS {
            return Err(SockError::Privileged);
        }
        if self.port_taken(kind, port) {
            return Err(SockError::AddrInUse);
        }
        Ok(Some(port))
    }

    /// An ephemeral port nothing holds.
    pub(super) fn pick_port(&mut self, kind: Kind) -> Option<u16> {
        for _ in 0..(u16::MAX - EPHEMERAL_FIRST) {
            let port = self.table.next_ephemeral();
            if !self.port_taken(kind, port) {
                return Some(port);
            }
        }
        None
    }

    /// Make a socket on interface `slot` that serves `port` on any address:
    /// a listener when `backlog` is above zero, else a datagram socket.
    pub(super) fn open_replica(
        units: &mut [Option<Unit>],
        slot: usize,
        owner: u64,
        port: u16,
        backlog: u32,
    ) -> Result<Replica, SockError> {
        let stack = &mut units
            .get_mut(slot)
            .and_then(Option::as_mut)
            .ok_or(SockError::Unreachable)?
            .stack;
        let kind = if backlog > 0 {
            Kind::Stream
        } else {
            Kind::Datagram
        };
        let sid = stack.socket_open(owner, kind)?;
        let any = SockAddr { addr: [0; 4], port };
        let made = stack.socket_bind(sid, owner, any).and_then(|()| {
            if backlog > 0 {
                stack.socket_listen(sid, owner, backlog)
            } else {
                Ok(())
            }
        });
        match made {
            Ok(()) => Ok(Replica { unit: slot, sid }),
            Err(error) => {
                let _ = stack.socket_close(sid, owner, 0);
                Err(error)
            }
        }
    }

    /// A new interface joined: every wildcard listener and datagram socket
    /// gets a replica on it (best effort: one that cannot is just absent
    /// there).
    pub(super) fn spread_to(&mut self, slot: usize) {
        let Net { units, table, .. } = self;
        for sock in table.iter_mut() {
            let (Shape::Spread { replicas, backlog }, Some(bound)) = (&mut sock.shape, sock.bound)
            else {
                continue;
            };
            if bound.only.is_some() {
                continue;
            }
            if let Ok(replica) = Net::open_replica(units, slot, sock.owner, bound.port, *backlog) {
                replicas.push(replica);
            }
        }
    }

    /// An interface went away: connections on it are lost, wildcard sockets
    /// lose their replica there, sockets tied to its address are lost, and its
    /// pings and lookups end.
    pub(super) fn forget_unit(&mut self, slot: usize) {
        self.forget_probes(slot);
        for sock in self.table.iter_mut() {
            if sock.bound.is_some_and(|bound| bound.only == Some(slot)) {
                sock.shape = Shape::Lost;
                continue;
            }
            match &mut sock.shape {
                Shape::Pinned(replica) if replica.unit == slot => sock.shape = Shape::Lost,
                Shape::Spread { replicas, .. } => replicas.retain(|r| r.unit != slot),
                _ => {}
            }
        }
    }
}
