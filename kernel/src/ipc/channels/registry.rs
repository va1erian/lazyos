//! The channel registry, indexed (docs/performance-plan.md P6.3).
//!
//! Channels live in a fixed table of [`MAX_CHANNELS`] slots. A channel id
//! carries its slot in its low [`INDEX_BITS`] bits above a sequence number
//! that never repeats, and a transaction id carries the slot of its channel
//! the same way, so a lookup by either is one index plus one comparison
//! instead of a scan of every channel (and, for a transaction, of every
//! channel's transactions). A slot reused by a new channel gets a new
//! sequence number: a stale channel or transaction id never matches it.

use super::*;

/// Low bits of a channel or transaction id that name the registry slot.
const INDEX_BITS: u32 = MAX_CHANNELS.trailing_zeros();
const INDEX_MASK: u64 = (1 << INDEX_BITS) - 1;
const _: () = assert!(MAX_CHANNELS.is_power_of_two());

/// Sequence numbers of channel and transaction ids. They start at 1, so no
/// id (and no handle's object id) is ever 0.
static NEXT_CHANNEL_SEQ: AtomicU64 = AtomicU64::new(1);
static NEXT_TXN_SEQ: AtomicU64 = AtomicU64::new(1);

/// The registry slot an id names.
fn index_of(id: u64) -> usize {
    (id & INDEX_MASK) as usize
}

/// A fresh transaction id on `channel_id`'s channel.
pub(super) fn new_txn_id(channel_id: u64) -> u64 {
    (NEXT_TXN_SEQ.fetch_add(1, Ordering::Relaxed) << INDEX_BITS) | (channel_id & INDEX_MASK)
}

/// Words of the occupancy bitmap, one bit per slot.
const WORDS: usize = MAX_CHANNELS / 64;
const _: () = assert!(MAX_CHANNELS.is_multiple_of(64));

/// Whether slot `index` is marked live in `occupied`.
fn occupied_at(occupied: &[u64; WORDS], index: usize) -> bool {
    occupied[index / 64] & (1 << (index % 64)) != 0
}

/// Every live channel, by slot.
pub(super) struct Registry {
    slots: [Option<Channel>; MAX_CHANNELS],
    /// One bit per live slot. A walk over the channels reads this rather
    /// than every slot, so it costs the live channels, not the table (a slot
    /// is a few hundred bytes; the scans run in syscalls, interrupts off).
    occupied: [u64; WORDS],
    /// The uid each live slot's `Resource::Channels` charge belongs to.
    owners: [u32; MAX_CHANNELS],
    live: usize,
}

impl Registry {
    pub(super) const fn new() -> Self {
        Registry {
            slots: [const { None }; MAX_CHANNELS],
            occupied: [0; WORDS],
            owners: [0; MAX_CHANNELS],
            live: 0,
        }
    }

    /// Live channels.
    pub(super) fn len(&self) -> usize {
        self.live
    }

    /// Install the channel `build` makes for a fresh id, charged to the uid
    /// of task `owner`; `RegistryFull` when every slot is taken and
    /// `ChannelQuota` when that uid holds its share of them.
    pub(super) fn insert(
        &mut self,
        owner: usize,
        build: impl FnOnce(u64) -> Channel,
    ) -> Result<u64, Error> {
        let word = self
            .occupied
            .iter()
            .position(|word| *word != u64::MAX)
            .ok_or(Error::RegistryFull)?;
        let index = word * 64 + self.occupied[word].trailing_ones() as usize;
        let uid = credentials::of(owner).uid;
        quota::charge(uid, Resource::Channels, 1).map_err(|_| Error::ChannelQuota)?;
        self.owners[index] = uid;
        let seq = NEXT_CHANNEL_SEQ.fetch_add(1, Ordering::Relaxed);
        let id = (seq << INDEX_BITS) | index as u64;
        self.slots[index] = Some(build(id));
        self.occupied[word] |= 1 << (index % 64);
        self.live += 1;
        Ok(id)
    }

    /// Remove channel `id`, if it is live.
    pub(super) fn remove(&mut self, id: u64) -> Option<Channel> {
        let slot = self.slots.get_mut(index_of(id))?;
        if slot.as_ref().is_some_and(|channel| channel.id == id) {
            let index = index_of(id);
            self.live -= 1;
            self.occupied[index / 64] &= !(1 << (index % 64));
            quota::release(self.owners[index], Resource::Channels, 1);
            return slot.take();
        }
        None
    }

    /// Drop every channel.
    pub(super) fn clear(&mut self) {
        for (slot, owner) in self.slots.iter_mut().zip(self.owners) {
            if slot.take().is_some() {
                quota::release(owner, Resource::Channels, 1);
            }
        }
        self.occupied = [0; WORDS];
        self.live = 0;
    }

    pub(super) fn get(&self, id: u64) -> Option<&Channel> {
        self.slots[index_of(id)]
            .as_ref()
            .filter(|channel| channel.id == id)
    }

    pub(super) fn get_mut(&mut self, id: u64) -> Option<&mut Channel> {
        self.slots[index_of(id)]
            .as_mut()
            .filter(|channel| channel.id == id)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &Channel> {
        let occupied = self.occupied;
        self.slots
            .iter()
            .enumerate()
            .filter(move |(index, _)| occupied_at(&occupied, *index))
            .filter_map(|(_, slot)| slot.as_ref())
    }

    pub(super) fn iter_mut(&mut self) -> impl Iterator<Item = &mut Channel> {
        let occupied = self.occupied;
        self.slots
            .iter_mut()
            .enumerate()
            .filter(move |(index, _)| occupied_at(&occupied, *index))
            .filter_map(|(_, slot)| slot.as_mut())
    }

    /// The channel holding transaction `txn_id`, and the transaction's index
    /// in its table.
    pub(super) fn txn(&self, txn_id: u64) -> Option<(&Channel, usize)> {
        let channel = self.slots[index_of(txn_id)].as_ref()?;
        let index = channel.txns.iter().position(|txn| txn.id == txn_id)?;
        Some((channel, index))
    }

    /// [`Registry::txn`], mutable.
    pub(super) fn txn_mut(&mut self, txn_id: u64) -> Option<(&mut Channel, usize)> {
        let channel = self.slots[index_of(txn_id)].as_mut()?;
        let index = channel.txns.iter().position(|txn| txn.id == txn_id)?;
        Some((channel, index))
    }
}
