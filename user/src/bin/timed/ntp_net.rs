//! What the network time client needs from the network stack: whether an
//! address exists, told by `netd`'s retained `system/net/<if>/addr` topic
//! (the doorbell that wakes the service) and confirmed with the stack's own
//! `Addresses` answer, and the two clients an exchange sends through.
//!
//! Everything is opened lazily and dropped on the first failure, like
//! `state.rs`'s links: `timed` starts before `netd` (and on an image with no
//! `netd` there is none), so a missing peer is retried quietly, not an error.

use alloc::rc::Rc;
use alloc::vec::Vec;

use user::central::{Bus, Subscription};
use user::messenger::{self};
use user::messenger::{netsock, netstack, Endpoint, EXPIRED_DEADLINE};
use user::sys;

/// Every interface's address, retained, so a late start sees the present
/// state and a lost lease arrives as an all-zero address.
const ADDR_FILTER: &str = "system/net/+/addr";

/// Ticks (100 Hz) between attempts to subscribe while that keeps failing:
/// every attempt builds a broker connection with a buffer the heap keeps.
const RETRY_TICKS: u64 = 3000;

pub(super) struct NetWatch {
    subscription: Option<Subscription>,
    bell: Option<Endpoint>,
    stack: Option<netstack::Client>,
    /// One socket-service client for the life of the service: the client owns
    /// a 20 KiB reply buffer and the heap never gives it back.
    sockets: Option<Rc<netsock::Client>>,
    buffer: Vec<u8>,
    next_subscribe_try: u64,
}

impl NetWatch {
    pub(super) fn new() -> NetWatch {
        NetWatch {
            subscription: None,
            bell: None,
            stack: None,
            sockets: None,
            buffer: alloc::vec![0u8; messenger::DEFAULT_BUFFER],
            next_subscribe_try: 0,
        }
    }

    /// The address topic's doorbell, to park on beside the service endpoint.
    pub(super) fn bell(&self) -> Option<Endpoint> {
        self.bell
    }

    /// Subscribe if not subscribed. Quiet when the broker is not up yet.
    pub(super) fn ensure(&mut self) {
        if self.subscription.is_some() || sys::clock() < self.next_subscribe_try {
            return;
        }
        self.next_subscribe_try = sys::clock() + RETRY_TICKS;
        let Ok(mut bus) = Bus::connect() else {
            return;
        };
        let Ok(mut subscription) = bus.subscribe(ADDR_FILTER) else {
            return;
        };
        match subscription.bell() {
            Ok(bell) => {
                self.bell = Some(bell);
                self.subscription = Some(subscription);
                self.next_subscribe_try = 0;
            }
            Err(_) => {
                let _ = subscription.unsubscribe();
            }
        }
    }

    /// Give the subscription up (the client was switched off): its doorbell
    /// must not stay in the service's wait set, unread messages and all.
    pub(super) fn release(&mut self) {
        self.bell = None;
        if let Some(subscription) = self.subscription.take() {
            let _ = subscription.unsubscribe();
        }
    }

    /// Drain the topic when its bell `rung`; returns whether anything came.
    /// The drain ends with an empty pull, which re-arms the bell.
    pub(super) fn drain(&mut self, rung: bool) -> bool {
        let Some(subscription) = &self.subscription else {
            return false;
        };
        if !rung {
            return false;
        }
        subscription.take_ring(&mut self.buffer);
        let mut any = false;
        loop {
            match subscription.recv_with(&mut self.buffer, Some(EXPIRED_DEADLINE)) {
                Ok(Some(_)) => any = true,
                Ok(None) => break,
                Err(_) => {
                    // The broker went away: subscribe again on the next pass.
                    self.bell = None;
                    self.subscription = None;
                    return true;
                }
            }
        }
        any
    }

    /// Both clients, opened on first use. `None` while `netd` is not
    /// registered.
    pub(super) fn clients(&mut self) -> Option<(&netstack::Client, &Rc<netsock::Client>)> {
        if self.stack.is_none() {
            self.stack = netstack::Client::connect().ok();
        }
        if self.sockets.is_none() {
            self.sockets = netsock::Client::connect().ok().map(Rc::new);
        }
        Some((self.stack.as_ref()?, self.sockets.as_ref()?))
    }

    /// Whether the stack holds an address right now; `None` when it cannot
    /// be asked (no `netd`, or it stopped answering: the clients are dropped
    /// so the next try resolves it again).
    pub(super) fn has_address(&mut self) -> Option<bool> {
        let (stack, _) = self.clients()?;
        match stack.addresses() {
            Ok(list) => Some(
                list.iter()
                    .any(|a| a.addr.len() == 4 && a.addr.iter().any(|&b| b != 0)),
            ),
            Err(_) => {
                self.stack = None;
                self.sockets = None;
                None
            }
        }
    }
}
