//! Datagram operations: a socket bound to any address owns a replica on every
//! interface and receives on all of them; each datagram it sends leaves by the
//! interface that routes its destination. A connected socket is pinned to the
//! interface that routes its peer.

use alloc::vec::Vec;

use crate::config::is_usable_unicast;
use crate::stack::{Kind, SockAddr, SockError, MAX_CHUNK, UDP_PAYLOAD};

use super::table::{Bound, Replica, Shape};
use super::Net;

impl Net {
    /// Give a fresh datagram socket its replicas (picking an ephemeral port
    /// if `Bind` was not called). A socket that has them already is left
    /// alone.
    pub(super) fn datagram_spread(&mut self, id: u32, owner: u64) -> Result<(), SockError> {
        let entry = self.table.entry(id, owner)?;
        match entry.shape {
            Shape::Fresh => {}
            Shape::Lost => return Err(SockError::Unreachable),
            Shape::Pinned(_) | Shape::Spread { .. } => return Ok(()),
        }
        let bound = entry.bound;
        let port = match bound {
            Some(bound) => bound.port,
            None => self.pick_port(Kind::Datagram).ok_or(SockError::AddrInUse)?,
        };
        let only = bound.and_then(|b| b.only);
        let slots: Vec<usize> = match only {
            Some(slot) => alloc::vec![slot],
            None => self.units().map(|(slot, _)| slot).collect(),
        };
        let mut replicas = Vec::new();
        for slot in slots {
            match Net::open_replica(&mut self.units, slot, owner, port, 0) {
                Ok(replica) => replicas.push(replica),
                Err(error) => {
                    self.close_replicas(&replicas, owner);
                    return Err(error);
                }
            }
        }
        let entry = self.table.entry(id, owner)?;
        entry.bound = Some(Bound { port, only });
        entry.shape = Shape::Spread {
            replicas,
            backlog: 0,
        };
        Ok(())
    }

    /// `Connect` on a datagram socket: pin it to the interface that routes
    /// `peer` and remember the peer there.
    pub(super) fn datagram_connect(
        &mut self,
        id: u32,
        owner: u64,
        peer: SockAddr,
    ) -> Result<(), SockError> {
        let entry = self.table.entry(id, owner)?;
        let (bound, shape) = (entry.bound, entry.shape.clone());
        let slot = match bound.and_then(|b| b.only) {
            Some(slot) => slot,
            None => self.route_for(peer.addr).ok_or(SockError::Unreachable)?,
        };
        let (keep, others) = match shape {
            Shape::Fresh => (None, Vec::new()),
            Shape::Pinned(replica) => (Some(replica), Vec::new()),
            Shape::Spread { replicas, .. } => {
                let keep = replicas.iter().copied().find(|r| r.unit == slot);
                let others = replicas.into_iter().filter(|r| r.unit != slot).collect();
                (keep, others)
            }
            Shape::Lost => return Err(SockError::Unreachable),
        };
        let replica = match keep {
            Some(replica) => replica,
            None => match bound {
                // A bound port is held in every interface of the socket.
                Some(bound) => Net::open_replica(&mut self.units, slot, owner, bound.port, 0)?,
                None => Replica {
                    unit: slot,
                    sid: self.stack(slot)?.socket_open(owner, Kind::Datagram)?,
                },
            },
        };
        let connected = self
            .stack(replica.unit)?
            .socket_connect(replica.sid, owner, peer);
        if let Err(error) = connected {
            if keep.is_none() {
                self.close_replicas(&[replica], owner);
            }
            return Err(error);
        }
        self.close_replicas(&others, owner);
        self.table.entry(id, owner)?.shape = Shape::Pinned(replica);
        Ok(())
    }

    /// `SendTo`: one datagram to `to`, by the interface that routes it. An
    /// unbound socket takes an ephemeral port first.
    pub fn socket_sendto(
        &mut self,
        id: u32,
        owner: u64,
        to: SockAddr,
        data: &[u8],
    ) -> Result<usize, SockError> {
        if data.len() > UDP_PAYLOAD {
            return Err(SockError::MessageSize);
        }
        if to.port == 0 || !is_usable_unicast(to.addr) || to.addr == [255; 4] {
            return Err(SockError::BadAddress);
        }
        if self.table.entry(id, owner)?.kind != Kind::Datagram {
            return Err(SockError::InvalidState);
        }
        self.datagram_spread(id, owner)?;
        let replica = match self.table.entry(id, owner)?.shape.clone() {
            Shape::Pinned(replica) => replica,
            Shape::Spread { replicas, .. } => {
                let slot = self
                    .route_among(to.addr, |slot| replicas.iter().any(|r| r.unit == slot))
                    .ok_or(SockError::Unreachable)?;
                replicas
                    .into_iter()
                    .find(|r| r.unit == slot)
                    .ok_or(SockError::Unreachable)?
            }
            Shape::Fresh | Shape::Lost => return Err(SockError::Unreachable),
        };
        self.stack(replica.unit)?
            .socket_sendto(replica.sid, owner, to, data)
    }

    /// `RecvFrom`: the oldest datagram (cut to `max`) and who sent it, or
    /// `Ok(None)` when none is queued on any interface.
    pub fn socket_recvfrom(
        &mut self,
        id: u32,
        owner: u64,
        max: usize,
    ) -> Result<Option<(Vec<u8>, SockAddr)>, SockError> {
        if max == 0 || max > MAX_CHUNK {
            return Err(SockError::BadAddress);
        }
        if self.table.entry(id, owner)?.kind != Kind::Datagram {
            return Err(SockError::InvalidState);
        }
        self.datagram_recv(id, owner, max)
    }

    pub(super) fn datagram_recv(
        &mut self,
        id: u32,
        owner: u64,
        max: usize,
    ) -> Result<Option<(Vec<u8>, SockAddr)>, SockError> {
        let mut replicas = match self.table.entry(id, owner)?.shape.clone() {
            Shape::Pinned(replica) => alloc::vec![replica],
            Shape::Spread { replicas, .. } => replicas,
            Shape::Fresh => return Ok(None),
            Shape::Lost => return Err(SockError::Reset),
        };
        if !replicas.is_empty() {
            self.turn = self.turn.wrapping_add(1);
            let first = self.turn % replicas.len();
            replicas.rotate_left(first);
        }
        for replica in replicas {
            if let Some(got) = self
                .stack(replica.unit)?
                .socket_recvfrom(replica.sid, owner, max)?
            {
                return Ok(Some(got));
            }
        }
        Ok(None)
    }
}
