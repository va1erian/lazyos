//! Oneway messages that did not fit webd's inbox. The inbox is shared by every client of webd and
//! is 64 messages deep, so a oneway send can meet `-EAGAIN`. Most messages may be lost then (a mouse
//! move, a wheel turn, a refresh: the next one says the same), but three kinds must arrive or the
//! view goes wrong for good, and the client keeps them until they do:
//!
//! * `ReleaseSlot`: webd treats a slot as busy until the client releases it, so a lost release takes
//!   a slot out of use for good (three of them and the view never paints again until `Resize`);
//! * a **release edge** (a mouse button up, a key up): a lost one leaves the button or key held in
//!   the page;
//! * a **state** (visible, focus): the last value must reach webd, an earlier one is stale.
//!
//! They are sent again in order at the start of every [`crate::LightWebView::poll`] and before any
//! later message (a later message never overtakes a release it was sent after). webd's side is
//! idempotent: releasing a free slot is a no-op and a repeated state is the same state. The app
//! polls at least once a second whatever happens, because webd's `Ping` wakes it.

use std::collections::VecDeque;

use messenger_generated::os_lazy_webd_light_v1 as wire;

use crate::event::Lost;
use crate::transport::{errno, Transport};
use crate::view::{LightWebView, SLOTS};

/// Most release edges and states kept while the inbox stays full, oldest dropped first: a client
/// that presses and releases keys faster than webd reads is not going to be heard in order anyway.
const KEPT: usize = 64;

/// What is owed to webd.
#[derive(Debug, Default)]
pub(crate) struct Outbox {
    /// `ReleaseSlot` is owed for this slot number.
    slots: [bool; SLOTS],
    /// Release edges and states, `(method, body)`, in the order they were made.
    queue: VecDeque<(u32, Vec<u8>)>,
}

impl Outbox {
    pub(crate) fn is_empty(&self) -> bool {
        !self.slots.contains(&true) && self.queue.is_empty()
    }

    /// Forget everything: the view is gone, or the messages named a view that is.
    pub(crate) fn clear(&mut self) {
        *self = Outbox::default();
    }

    /// Forget the owed slot releases: every slot was detached (`Resize`) and a release sent later
    /// would name a new slot of the same number.
    pub(crate) fn forget_slots(&mut self) {
        self.slots = [false; SLOTS];
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

/// How one try ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sent {
    Yes,
    /// webd's inbox is full.
    Full,
    /// The view or the service is gone, or the message was refused for good: nothing to keep.
    Gone,
}

impl<T: Transport> LightWebView<T> {
    /// A oneway message that may be lost: sent if the inbox has room and nothing owed is waiting
    /// ahead of it, else dropped (a mouse move, a wheel turn).
    pub(crate) fn send(&mut self, method: u32, body: Vec<u8>) {
        self.flush_outbox();
        if self.outbox.is_empty() {
            self.try_send(method, body);
        }
    }

    /// A release edge: sent, or kept and sent again until it arrives.
    pub(crate) fn send_edge(&mut self, method: u32, body: Vec<u8>) {
        self.send_owed(method, body, false);
    }

    /// A state (visible, focus): sent, or kept until it arrives, replacing an older one of its kind.
    pub(crate) fn send_state(&mut self, method: u32, body: Vec<u8>) {
        self.send_owed(method, body, true);
    }

    fn send_owed(&mut self, method: u32, body: Vec<u8>, latest_wins: bool) {
        self.flush_outbox();
        if self.outbox.is_empty() && self.try_send(method, body.clone()) != Sent::Full {
            return;
        }
        if self.lost.is_none() {
            self.outbox.keep(method, body, latest_wins);
        }
    }

    /// Give a slot back to webd: sent now, or owed until it is.
    pub(crate) fn owe_release(&mut self, slot: u32) {
        if let Some(owed) = self.outbox.slots.get_mut(slot as usize) {
            *owed = true;
        }
        self.flush_outbox();
    }

    /// Send what is owed, in order, until the inbox is full again.
    pub(crate) fn flush_outbox(&mut self) {
        if self.outbox.is_empty() {
            return;
        }
        let Some(view) = self.live.as_ref().map(|live| live.view) else {
            self.outbox.clear();
            return;
        };
        for slot in 0..SLOTS {
            if !self.outbox.slots[slot] {
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
            self.outbox.slots[slot] = false;
        }
        while let Some((method, body)) = self.outbox.queue.front().cloned() {
            if self.try_send(method, body) == Sent::Full {
                return;
            }
            self.outbox.queue.pop_front();
        }
    }

    /// One try. A service that is gone turns the view lost; nothing else can go wrong that the
    /// caller could do anything about.
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
                    self.outbox.clear();
                }
                Sent::Gone
            }
        }
    }
}
