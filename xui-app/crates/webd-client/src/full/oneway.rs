//! A full view's oneway messages that must arrive when webd's inbox is full (`-EAGAIN`), as for a
//! light view (`crate::outbox`): `ReleaseSlot` (a lost one takes a slot out of use for good), a
//! button or key release, and the last visible and focus states. They are sent again, in order,
//! at the start of every [`FullWebView::poll`] and before any later message. A message that may be
//! lost (a mouse move, a wheel turn, a refresh) is dropped when it does not fit.

use std::collections::VecDeque;

use messenger_generated::os_lazy_webd_view_v1 as wire;

use super::{FullWebView, ALL_SLOTS};
use crate::event::Lost;
use crate::transport::{errno, ScopedTransport};

/// Release edges and states kept while the inbox stays full, oldest dropped first.
const KEPT: usize = 64;

/// What is owed to webd.
#[derive(Debug, Default)]
pub(super) struct Owed {
    /// `ReleaseSlot` is owed for this slot number.
    slots: [bool; ALL_SLOTS],
    /// Release edges and states, `(method, body)`, in the order they were made.
    queue: VecDeque<(u32, Vec<u8>)>,
}

impl Owed {
    fn is_empty(&self) -> bool {
        !self.slots.contains(&true) && self.queue.is_empty()
    }

    pub(super) fn clear(&mut self) {
        *self = Owed::default();
    }

    /// Every slot was detached (`Resize`): a release sent later would name a new slot.
    pub(super) fn forget_slots(&mut self) {
        self.slots = [false; ALL_SLOTS];
    }

    fn keep(&mut self, method: u32, body: Vec<u8>, latest_wins: bool) {
        if latest_wins {
            self.queue.retain(|(kept, _)| *kept != method);
        }
        if self.queue.len() >= KEPT {
            self.queue.pop_front();
        }
        self.queue.push_back((method, body));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sent {
    Yes,
    Full,
    Gone,
}

impl<T: ScopedTransport> FullWebView<T> {
    /// A message that may be lost: sent if the inbox has room and nothing owed waits ahead of it.
    pub(super) fn send(&mut self, method: u32, body: Vec<u8>) {
        self.flush_owed();
        if self.owed.is_empty() {
            self.try_send(method, body);
        }
    }

    /// A release edge, or (`latest_wins`) a state: sent, or kept until it arrives.
    pub(super) fn send_owed(&mut self, method: u32, body: Vec<u8>, latest_wins: bool) {
        self.flush_owed();
        if self.owed.is_empty() && self.try_send(method, body.clone()) != Sent::Full {
            return;
        }
        if self.lost.is_none() {
            self.owed.keep(method, body, latest_wins);
        }
    }

    /// Give a slot back to webd: sent now, or owed until it is.
    pub(super) fn owe_release(&mut self, slot: u32) {
        if self.live.is_none() {
            return;
        }
        if let Some(owed) = self.owed.slots.get_mut(slot as usize) {
            *owed = true;
        }
        self.flush_owed();
    }

    /// Send what is owed, in order, until the inbox is full again.
    pub(super) fn flush_owed(&mut self) {
        if self.owed.is_empty() {
            return;
        }
        let Some(view) = self.view_id() else {
            self.owed.clear();
            return;
        };
        for slot in 0..ALL_SLOTS {
            if !self.owed.slots[slot] {
                continue;
            }
            let args = wire::ReleaseSlotArgs {
                view,
                slot: slot as u32,
            };
            let sent = match wire::encode_release_slot_args(&args) {
                Ok(body) => self.try_send(wire::METHOD_RELEASESLOT, body),
                Err(_) => Sent::Gone,
            };
            if sent == Sent::Full {
                return;
            }
            self.owed.slots[slot] = false;
        }
        while let Some((method, body)) = self.owed.queue.front().cloned() {
            if self.try_send(method, body) == Sent::Full {
                return;
            }
            self.owed.queue.pop_front();
        }
    }

    fn try_send(&mut self, method: u32, body: Vec<u8>) -> Sent {
        if self.lost.is_some() {
            return Sent::Gone;
        }
        match self.transport.send(method, body) {
            Ok(()) => Sent::Yes,
            Err(code) if -code == errno::EAGAIN => Sent::Full,
            Err(code) => {
                if matches!(-code, errno::ENOENT | errno::EPIPE) {
                    self.lost = Some(Lost::ServiceGone);
                    self.owed.clear();
                }
                Sent::Gone
            }
        }
    }
}
