//! The controller's event queue: waiting for one event while keeping the
//! others for later takers, and moving new events off the event ring
//! (xHCI 4.9.4). Split out of `hc.rs`.

use user::sys;
use xhci::regs::{rt, Mmio};
use xhci::trb::{kind, Trb};

use super::{nap, Hc, PENDING_CAP, TIMEOUT_TICKS};
use crate::Error;

impl Hc {
    /// Wait for the first event matching `wanted`, keeping the others.
    pub(crate) fn wait(&mut self, wanted: impl Fn(&Trb) -> bool) -> Result<Trb, Error> {
        self.wait_until(sys::clock() + TIMEOUT_TICKS, wanted)
    }

    /// [`Hc::wait`] with the caller's deadline (absolute ticks): a mass
    /// storage transfer may legitimately take longer than a command.
    pub(crate) fn wait_until(
        &mut self,
        deadline: u64,
        wanted: impl Fn(&Trb) -> bool,
    ) -> Result<Trb, Error> {
        loop {
            self.pump();
            if let Some(at) = self.pending.iter().position(&wanted) {
                return Ok(self.pending.remove(at).unwrap_or_default());
            }
            if sys::clock() > deadline {
                return Err(Error::Timeout("event"));
            }
            nap();
        }
    }

    /// Move every new event into the pending queue and tell the controller
    /// how far the driver got.
    pub(crate) fn pump(&mut self) {
        let mut moved = false;
        while let Some(event) = self.events.pop() {
            if self.pending.len() == PENDING_CAP {
                self.pending.pop_front();
                self.dropped += 1;
            }
            self.pending.push_back(event);
            moved = true;
        }
        if moved {
            let erdp = self.events.erdp();
            let at = self.rt + rt::INTERRUPTERS + rt::ERDP;
            self.bar.write64(at, erdp);
        }
    }

    /// Drop every queued transfer event of `slot` (`dci` 0: all of its
    /// endpoints). Called after Disable Slot, or after an endpoint was reset
    /// and its ring skipped: the memory is reused at the same bus addresses,
    /// so a stale completion could otherwise look like a new one.
    pub(crate) fn discard(&mut self, slot: u8, dci: u8) {
        self.pump();
        self.pending.retain(|event| {
            !(event.kind() == kind::TRANSFER_EVENT
                && event.slot() == slot
                && (dci == 0 || event.endpoint() == dci))
        });
    }

    /// Take the oldest pending event, if any.
    pub(crate) fn next_event(&mut self) -> Option<Trb> {
        self.pump();
        self.pending.pop_front()
    }
}
