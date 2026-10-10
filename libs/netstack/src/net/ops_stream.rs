//! Stream operations: connect on the interface the route picks, listen on
//! every interface, accept from any replica, and the I/O on a connection,
//! which lives on one interface.

use alloc::vec::Vec;

use crate::config::is_usable_unicast;
use crate::stack::{ready, Kind, Received, SockAddr, SockError, MAX_BACKLOG, MAX_CHUNK};

use super::table::{Bound, Replica, Shape};
use super::Net;

impl Net {
    /// `Connect`: for a stream, pick the interface that routes `peer` and
    /// start the handshake there (poll [`Net::socket_connect_status`]); for a
    /// datagram socket, fix its default peer.
    pub fn socket_connect(&mut self, id: u32, owner: u64, peer: SockAddr) -> Result<(), SockError> {
        if peer.port == 0 || !is_usable_unicast(peer.addr) || peer.addr == [255; 4] {
            return Err(SockError::BadAddress);
        }
        let entry = self.table.entry(id, owner)?;
        if entry.kind == Kind::Datagram {
            return self.datagram_connect(id, owner, peer);
        }
        let bound = match entry.shape {
            Shape::Fresh => entry.bound,
            Shape::Lost => return Err(SockError::Unreachable),
            Shape::Pinned(_) | Shape::Spread { .. } => return Err(SockError::InvalidState),
        };
        let slot = match bound.and_then(|b| b.only) {
            Some(slot) => self
                .unit(slot)
                .filter(|unit| unit.usable())
                .map(|_| slot)
                .ok_or(SockError::Unreachable)?,
            None => self.route_for(peer.addr).ok_or(SockError::Unreachable)?,
        };
        let stack = self.stack(slot)?;
        let sid = stack.socket_open(owner, Kind::Stream)?;
        let any = |port| SockAddr { addr: [0; 4], port };
        let started = match bound {
            Some(bound) => stack.socket_bind(sid, owner, any(bound.port)),
            None => Ok(()),
        }
        .and_then(|()| stack.socket_connect(sid, owner, peer));
        if let Err(error) = started {
            let _ = stack.socket_close(sid, owner, 0);
            return Err(error);
        }
        self.table.entry(id, owner)?.shape = Shape::Pinned(Replica { unit: slot, sid });
        Ok(())
    }

    /// Whether a `Connect` finished: `Ok(true)` established, `Ok(false)` still
    /// trying, `Err(Refused)` reset by the peer.
    pub fn socket_connect_status(&mut self, id: u32, owner: u64) -> Result<bool, SockError> {
        let entry = self.table.entry(id, owner)?;
        match entry.shape.clone() {
            Shape::Pinned(replica) => self
                .stack(replica.unit)?
                .socket_connect_status(replica.sid, owner),
            Shape::Fresh => Err(SockError::NotConnected),
            Shape::Spread { .. } if entry.kind == Kind::Datagram => Err(SockError::NotConnected),
            Shape::Spread { .. } => Err(SockError::InvalidState),
            Shape::Lost => Err(SockError::Reset),
        }
    }

    /// `Listen`: turn a bound (or fresh) stream socket into a listener on
    /// every interface, or on the one whose address it was bound to.
    pub fn socket_listen(&mut self, id: u32, owner: u64, backlog: u32) -> Result<(), SockError> {
        if !(1..=MAX_BACKLOG as u32).contains(&backlog) {
            return Err(SockError::BadAddress);
        }
        let entry = self.table.entry(id, owner)?;
        if entry.kind != Kind::Stream || !matches!(entry.shape, Shape::Fresh) {
            return Err(SockError::InvalidState);
        }
        let bound = entry.bound;
        let port = match bound {
            Some(bound) => bound.port,
            None => self.pick_port(Kind::Stream).ok_or(SockError::AddrInUse)?,
        };
        let only = bound.and_then(|b| b.only);
        let slots: Vec<usize> = match only {
            Some(slot) => alloc::vec![slot],
            None => self.units().map(|(slot, _)| slot).collect(),
        };
        let mut replicas = Vec::new();
        for slot in slots {
            match Net::open_replica(&mut self.units, slot, owner, port, backlog) {
                Ok(replica) => replicas.push(replica),
                Err(error) => {
                    self.close_replicas(&replicas, owner);
                    return Err(error);
                }
            }
        }
        let entry = self.table.entry(id, owner)?;
        entry.bound = Some(Bound { port, only });
        entry.shape = Shape::Spread { replicas, backlog };
        Ok(())
    }

    /// Close replicas that never carried anything.
    pub(super) fn close_replicas(&mut self, replicas: &[Replica], owner: u64) {
        for replica in replicas {
            if let Ok(stack) = self.stack(replica.unit) {
                let _ = stack.socket_close(replica.sid, owner, 0);
            }
        }
    }

    /// `Accept`: one established connection as a new socket of `owner`, or
    /// `Ok(None)` when none is waiting.
    pub fn socket_accept(
        &mut self,
        id: u32,
        owner: u64,
    ) -> Result<Option<(u32, SockAddr)>, SockError> {
        self.socket_accept_as(id, owner, owner)
    }

    /// [`Net::socket_accept`] with the new socket given to `new_owner`.
    pub fn socket_accept_as(
        &mut self,
        id: u32,
        owner: u64,
        new_owner: u64,
    ) -> Result<Option<(u32, SockAddr)>, SockError> {
        let entry = self.table.entry(id, owner)?;
        let Shape::Spread { replicas, backlog } = &entry.shape else {
            return Err(SockError::InvalidState);
        };
        if *backlog == 0 {
            return Err(SockError::InvalidState);
        }
        let mut replicas = replicas.clone();
        if !replicas.is_empty() {
            self.turn = self.turn.wrapping_add(1);
            let first = self.turn % replicas.len();
            replicas.rotate_left(first);
        }
        for replica in replicas {
            let stack = self.stack(replica.unit)?;
            if stack.socket_readiness(replica.sid, owner)? & ready::ACCEPTABLE == 0 {
                continue;
            }
            // The connection stays in the backlog until the caller has room.
            self.table.check_quota(new_owner)?;
            let stack = self.stack(replica.unit)?;
            let Some((conn, peer)) = stack.socket_accept_as(replica.sid, owner, new_owner)? else {
                continue;
            };
            let id = self.table.insert(
                new_owner,
                Kind::Stream,
                Shape::Pinned(Replica {
                    unit: replica.unit,
                    sid: conn,
                }),
            );
            self.sockets.opened += 1;
            self.sockets.accepted += 1;
            return Ok(Some((id, peer)));
        }
        Ok(None)
    }

    /// The connection behind a stream socket, for I/O.
    fn connection(&mut self, id: u32, owner: u64) -> Result<Replica, SockError> {
        let entry = self.table.entry(id, owner)?;
        match entry.shape {
            Shape::Pinned(replica) => Ok(replica),
            Shape::Fresh => Err(SockError::NotConnected),
            // A datagram socket with no default peer is just not connected.
            Shape::Spread { .. } if entry.kind == Kind::Datagram => Err(SockError::NotConnected),
            Shape::Spread { .. } => Err(SockError::InvalidState),
            Shape::Lost => Err(SockError::Reset),
        }
    }

    /// `Send`: how many bytes were queued (`Ok(0)` never happens: a full
    /// buffer is `WouldBlock`). On a datagram socket, a send to its peer.
    pub fn socket_send(&mut self, id: u32, owner: u64, data: &[u8]) -> Result<usize, SockError> {
        if data.is_empty() || data.len() > MAX_CHUNK {
            return Err(SockError::BadAddress);
        }
        let replica = self.connection(id, owner)?;
        self.stack(replica.unit)?
            .socket_send(replica.sid, owner, data)
    }

    /// `Recv`: up to `max` bytes, an empty vector at the end of the stream,
    /// `Ok(None)` when nothing is there yet. On a datagram socket, the next
    /// datagram without its sender.
    pub fn socket_recv(
        &mut self,
        id: u32,
        owner: u64,
        max: usize,
    ) -> Result<Option<Vec<u8>>, SockError> {
        if max == 0 || max > MAX_CHUNK {
            return Err(SockError::BadAddress);
        }
        if self.table.entry(id, owner)?.kind == Kind::Datagram {
            return Ok(self.datagram_recv(id, owner, max)?.map(|(data, _)| data));
        }
        let replica = self.connection(id, owner)?;
        self.stack(replica.unit)?
            .socket_recv(replica.sid, owner, max)
    }

    /// `Shutdown`: end reading (`read`), writing (`write`) or both.
    pub fn socket_shutdown(
        &mut self,
        id: u32,
        owner: u64,
        read: bool,
        write: bool,
    ) -> Result<(), SockError> {
        if self.table.entry(id, owner)?.kind == Kind::Datagram {
            return Err(SockError::InvalidState);
        }
        let replica = self.connection(id, owner)?;
        self.stack(replica.unit)?
            .socket_shutdown(replica.sid, owner, read, write)
    }

    /// Offer the free part of stream `id`'s send buffer to `fill`
    /// (`Stack::socket_send_with`).
    pub fn socket_send_with(
        &mut self,
        id: u32,
        owner: u64,
        max: usize,
        fill: impl FnOnce(&mut [u8]) -> usize,
    ) -> Result<usize, SockError> {
        let replica = self.connection(id, owner)?;
        self.stack(replica.unit)?
            .socket_send_with(replica.sid, owner, max, fill)
    }

    /// Offer the queued part of stream `id`'s receive buffer to `take`
    /// (`Stack::socket_recv_with`).
    pub fn socket_recv_with(
        &mut self,
        id: u32,
        owner: u64,
        max: usize,
        take: impl FnOnce(&[u8]) -> usize,
    ) -> Result<Received, SockError> {
        let replica = self.connection(id, owner)?;
        self.stack(replica.unit)?
            .socket_recv_with(replica.sid, owner, max, take)
    }
}
