//! The caller-visible socket table: ids, owners, quotas, the port the caller
//! chose and where the sockets behind an id live (the [`Shape`]).
//!
//! The rules are the per-interface table's (`stack/sockets.rs`): at most
//! [`MAX_SOCKETS`] open, [`MAX_PER_OWNER`] per owner, and an id carries a
//! generation above its slot so a stale id is `BadSocket`, never somebody
//! else's new socket.

use alloc::vec::Vec;

use crate::stack::{Kind, SockError, EPHEMERAL_FIRST, MAX_PER_OWNER, MAX_SOCKETS};

/// One smoltcp socket of one interface behind a caller-visible socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Replica {
    /// The interface's slot.
    pub unit: usize,
    /// The id in that interface's own table.
    pub sid: u32,
}

/// Where the sockets behind an id are.
#[derive(Clone, Debug)]
pub(super) enum Shape {
    /// Nothing made yet: an interface is chosen when a stream connects, a
    /// datagram socket binds or sends.
    Fresh,
    /// One socket on one interface: a connection, or a datagram socket that
    /// connected.
    Pinned(Replica),
    /// A listener (`backlog` > 0) or a datagram socket serving every
    /// interface, or the one it was bound to: one replica each.
    Spread {
        replicas: Vec<Replica>,
        backlog: u32,
    },
    /// The interface under the socket was removed.
    Lost,
}

/// What `Bind` chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Bound {
    pub port: u16,
    /// The slot of the interface whose address was named; `None`: any.
    pub only: Option<usize>,
}

pub(super) struct VSock {
    pub id: u32,
    pub owner: u64,
    pub kind: Kind,
    pub bound: Option<Bound>,
    pub shape: Shape,
}

pub(super) struct Table {
    slots: Vec<Option<VSock>>,
    generation: u32,
    next_ephemeral: u16,
}

impl Table {
    pub fn new(seed: u64) -> Table {
        Table {
            slots: (0..MAX_SOCKETS).map(|_| None).collect(),
            generation: 1,
            // A seeded offset, so a restart does not reuse the ports of the
            // connections it just lost.
            next_ephemeral: EPHEMERAL_FIRST + (seed as u16 % (u16::MAX - EPHEMERAL_FIRST)),
        }
    }

    fn slot_of(id: u32) -> usize {
        (id & 0xFF) as usize
    }

    pub fn open_count(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    pub fn owned_by(&self, owner: u64) -> usize {
        self.slots
            .iter()
            .flatten()
            .filter(|s| s.owner == owner)
            .count()
    }

    /// Every owner of an open socket, each once.
    pub fn owners(&self) -> Vec<u64> {
        let mut owners: Vec<u64> = self.slots.iter().flatten().map(|s| s.owner).collect();
        owners.sort_unstable();
        owners.dedup();
        owners
    }

    /// Every open id, for the tests.
    pub fn ids(&self) -> Vec<u32> {
        self.slots.iter().flatten().map(|s| s.id).collect()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut VSock> {
        self.slots.iter_mut().flatten()
    }

    /// The socket `id` names, if `owner` owns it.
    pub fn entry(&mut self, id: u32, owner: u64) -> Result<&mut VSock, SockError> {
        let entry = self
            .slots
            .get_mut(Table::slot_of(id))
            .and_then(Option::as_mut)
            .filter(|s| s.id == id)
            .ok_or(SockError::BadSocket)?;
        if entry.owner != owner {
            return Err(SockError::NotOwner);
        }
        Ok(entry)
    }

    pub fn check_quota(&self, owner: u64) -> Result<(), SockError> {
        if self.owned_by(owner) >= MAX_PER_OWNER {
            return Err(SockError::TooManyForOwner);
        }
        if self.open_count() >= MAX_SOCKETS {
            return Err(SockError::TooMany);
        }
        Ok(())
    }

    /// Put a socket in a free slot; the caller has passed [`Table::check_quota`].
    pub fn insert(&mut self, owner: u64, kind: Kind, shape: Shape) -> u32 {
        let slot = self
            .slots
            .iter()
            .position(Option::is_none)
            .expect("check_quota passed: a slot is free");
        let id = (slot as u32) | (self.generation << 8);
        // The generation never reaches zero again: a wrap skips it.
        self.generation = (self.generation.wrapping_add(1) & 0x00FF_FFFF).max(1);
        self.slots[slot] = Some(VSock {
            id,
            owner,
            kind,
            bound: None,
            shape,
        });
        id
    }

    /// Remove the socket `id` of `owner`.
    pub fn take(&mut self, id: u32, owner: u64) -> Result<VSock, SockError> {
        self.entry(id, owner)?;
        Ok(self.slots[Table::slot_of(id)].take().expect("checked"))
    }

    /// Remove every socket `owner` holds.
    pub fn take_owner(&mut self, owner: u64) -> Vec<VSock> {
        self.slots
            .iter_mut()
            .filter(|s| s.as_ref().is_some_and(|s| s.owner == owner))
            .filter_map(Option::take)
            .collect()
    }

    /// Whether a caller-visible socket of `kind` holds `port`.
    pub fn port_taken(&self, kind: Kind, port: u16) -> bool {
        self.slots
            .iter()
            .flatten()
            .any(|s| s.kind == kind && s.bound.is_some_and(|b| b.port == port))
    }

    /// The next ephemeral port candidate.
    pub fn next_ephemeral(&mut self) -> u16 {
        let port = self.next_ephemeral;
        self.next_ephemeral = if port == u16::MAX {
            EPHEMERAL_FIRST
        } else {
            port + 1
        };
        port
    }
}
