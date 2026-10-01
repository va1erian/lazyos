//! Who is calling, and whether they are still alive.
//!
//! Messenger stamps a request with the sender's **task slot**, and slots are
//! reused: a socket keyed by the slot alone would pass to whichever task next
//! lands there. The owner of a socket is `(pid << 16) | slot`, with the pid
//! read from the scheduler's task list at the time of the call.
//!
//! **Known gap.** Today the kernel's pid *is* the slot number, so the pid adds
//! nothing: a task that lands in a dead owner's slot before the sweep has run
//! (or while its predecessor's sockets are still open) is taken for that owner.
//! The fix is a per-slot spawn counter in the task snapshot; the owner id is
//! already shaped to carry it (the pid field) and nothing else here changes.
//!
//! The task list is one syscall and a 22 KiB copy; it is refreshed at most
//! once per tick, however many requests arrive.

use alloc::vec;
use alloc::vec::Vec;

use user::sys;
use user::task_snapshot::TaskSnapshot;

pub(super) struct Tasks {
    snapshot: Vec<u8>,
    /// The tick the snapshot was taken at (`None`: never, or the call failed).
    at: Option<u64>,
}

impl Tasks {
    pub(super) fn new() -> Tasks {
        Tasks {
            snapshot: vec![0u8; TaskSnapshot::SIZE],
            at: None,
        }
    }

    /// Make the snapshot at least as new as `tick`; false if the kernel
    /// would not give one.
    fn refresh(&mut self, tick: u64) -> bool {
        if self.at == Some(tick) {
            return true;
        }
        if sys::tasks(self.snapshot.as_mut_ptr() as u64) != 0 {
            self.at = None;
            return false;
        }
        self.at = Some(tick);
        true
    }

    /// The owner id of the task in `slot`, if it is alive.
    pub(super) fn owner_of(&mut self, slot: u64, tick: u64) -> Option<u64> {
        if !self.refresh(tick) {
            return None;
        }
        let pid = TaskSnapshot::live_pid(&self.snapshot, usize::try_from(slot).ok()?)?;
        Some((pid << 16) | slot)
    }

    /// Whether the task `owner` names is still the one in its slot. When the
    /// task list cannot be read, everyone is assumed alive (no reclaim is
    /// better than reclaiming a live client's sockets).
    pub(super) fn alive(&mut self, owner: u64, tick: u64) -> bool {
        if !self.refresh(tick) {
            return true;
        }
        let slot = (owner & 0xFFFF) as usize;
        TaskSnapshot::live_pid(&self.snapshot, slot) == Some(owner >> 16)
    }
}
