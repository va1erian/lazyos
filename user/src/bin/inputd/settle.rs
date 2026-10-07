//! Keys wait for the focus a click gives (`inputmap::barrier`).
//!
//! A press goes to the compositor as a `PointerEvent`, and the focus it
//! leads to comes back as `NoteFocus`, followed by `NoteInputDone` once the
//! compositor has handled it. Key content read in between is held, in order,
//! and delivered when that note arrives (or the hold times out), so it
//! reaches the window the click focused rather than the one it left.
//!
//! Keys and pointer records drained in the same pass are fed to separate
//! engines, so a key typed a few milliseconds *before* a press in that pass
//! is held too and goes to the new focus; nothing is lost or reordered.

use alloc::vec::Vec;

use inputmap::{Output, PointerOut};

use super::hub::Hub;

impl Hub {
    /// Hold what follows a forwarded button press (`out.buttons` gained a
    /// bit since the last event sent).
    pub(super) fn note_pointer_sent(&mut self, out: &PointerOut, now: u64) {
        if out.buttons & !self.pointer.sent_buttons != 0 {
            self.barrier.pressed(out.seq, now);
        }
        self.pointer.sent_buttons = out.buttons;
    }

    /// Deliver this pass's key outputs, unless a press is still being
    /// handled: then they join the hold. A hold that timed out is delivered
    /// first.
    pub(super) fn deliver_keys(&mut self, outputs: &mut Vec<Output>, now: u64) {
        let expired = self.barrier.expire(now);
        self.deliver(&expired);
        let early = self.barrier.hold(outputs);
        self.deliver(&early);
        self.deliver(outputs);
        outputs.clear();
    }

    /// `NoteInputDone`: the compositor handled the pointer up to `seq`, and
    /// the focus it noted before this is the one the held keys belong to.
    pub(super) fn input_done(&mut self, seq: u64) {
        let held = self.barrier.settled(seq);
        self.deliver(&held);
    }

    /// The compositor went away: nobody will answer, deliver what was held
    /// (to the console session, if any; see `Router::focused_session`).
    pub(super) fn release_keys(&mut self) {
        let held = self.barrier.release();
        self.deliver(&held);
    }

    /// When the service loop must wake to end a hold.
    pub(super) fn keys_due(&self) -> Option<u64> {
        self.barrier.next_due()
    }
}
