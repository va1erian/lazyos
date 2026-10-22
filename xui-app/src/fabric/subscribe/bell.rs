//! The subscription doorbell (`docs/performance-plan.md` P7.2): one channel
//! per subscription, one end handed to the broker in `Bell`, the other parked
//! on by the caller.

use super::{wire, Errno, SubId, Subscriber, Transport, Wait, ENOENT, MAX_BELL_MESSAGES};

/// What [`Subscriber::consume_ready`] found.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Rung {
    /// Subscriptions the broker said have events, each once, in the order
    /// their bells were first read. Drain them.
    pub ready: Vec<SubId>,
    /// Subscriptions whose doorbell is gone (the broker dropped its end, or
    /// the channel failed) with the errno; they receive no more rings, so
    /// the caller must fall back to an explicit refresh or resubscribe.
    pub lost: Vec<(SubId, Errno)>,
}

/// A bell message is a small one-way parcel; this is generous.
const BELL_BUFFER: usize = 512;

impl<T: Transport> Subscriber<T> {
    /// Give `id` a doorbell and return our end of it (the handle to park on).
    /// Asked once: a second call returns the same handle. The broker keeps
    /// the other end, which the kernel moves to it with the request; if the
    /// call fails both ends are closed here.
    pub fn attach_bell(&mut self, id: SubId) -> Result<u64, Errno> {
        let index = self.index(id)?;
        if let Some(bell) = self.subs[index].bell {
            return Ok(bell);
        }
        let (mine, theirs) = self.transport.create_pair()?;
        let installed = wire::bell_request(id, theirs)
            .and_then(|request| self.exchange(&request, Wait::Bounded));
        if let Err(code) = installed {
            // The transfer may not have happened. Closing a handle that did
            // move is a harmless `EBADF`.
            self.transport.close(theirs);
            self.transport.close(mine);
            return Err(code);
        }
        self.subs[index].bell = Some(mine);
        Ok(mine)
    }

    /// Our end of every attached doorbell, for a `wait_any` beside the
    /// caller's own endpoints (the kernel allows `WAIT_MAX_ENDPOINTS` words
    /// per wait: keep the number of live subscriptions below it).
    pub fn doorbell_handles(&self) -> Vec<u64> {
        self.subs.iter().filter_map(|sub| sub.bell).collect()
    }

    /// Read every message waiting on the doorbells (at most
    /// [`MAX_BELL_MESSAGES`] per bell) and say which subscriptions have
    /// events. Does not touch the broker. The bell is re-armed by the drain
    /// that follows, not by this call.
    pub fn consume_ready(&mut self) -> Rung {
        let mut rung = Rung::default();
        let mut buf = [0u8; BELL_BUFFER];
        let bells: Vec<(SubId, u64)> = self
            .subs
            .iter()
            .filter_map(|sub| sub.bell.map(|bell| (sub.id, bell)))
            .collect();
        for (owner, bell) in bells {
            for _ in 0..MAX_BELL_MESSAGES {
                match self.transport.recv(bell, &mut buf) {
                    Ok(Some(length)) => {
                        let named = buf.get(..length).and_then(wire::ready_subscription);
                        if let Some(id) = named.filter(|id| self.holds(*id)) {
                            if !rung.ready.contains(&id) {
                                rung.ready.push(id);
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(code) => {
                        self.lose_bell(owner, bell);
                        rung.lost.push((owner, code));
                        break;
                    }
                }
            }
        }
        rung
    }

    /// Forget a dead doorbell: close our end and clear the record.
    fn lose_bell(&mut self, owner: SubId, bell: u64) {
        self.transport.close(bell);
        if let Some(sub) = self.subs.iter_mut().find(|sub| sub.id == owner) {
            sub.bell = None;
        }
    }

    /// Whether `id` still has a doorbell attached.
    pub fn has_bell(&self, id: SubId) -> Result<bool, Errno> {
        self.sub(id).map(|sub| sub.bell.is_some()).ok_or(-ENOENT)
    }
}
