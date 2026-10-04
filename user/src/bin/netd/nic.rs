//! `netd`'s side of the NIC driver: find it, read `Info`, attach the rings and
//! keep them healthy.
//!
//! **One endpoint.** The notify endpoint handed to the driver is a handle to
//! `netd`'s own published service endpoint, so the driver's `Notify` messages
//! land in the same inbox as client calls and one `recv` with a deadline serves
//! both (docs/networking-plan.md section 6). A `Notify` is only ever a hint to
//! look at the rings; nothing in it is trusted, so a client forging one gains
//! nothing.
//!
//! **Failure.** The driver restarting, the ring being corrupted or the driver
//! going silent all end the same way: the rings are dropped, the stack keeps
//! its lease and timers, and this module attaches again to whatever driver
//! answers next.

use alloc::format;
use alloc::string::String;

use netstack::Stack;
use user::messenger::net::{self as nic, Client, Shared};
use user::messenger::registry;
use user::sys;

/// Slots per ring: 256 frames of burst each way (a 1 MiB buffer), more than
/// a 256 KiB TCP window in full-size segments, so a window's worth of
/// frames never waits in the driver for room (docs/performance-plan.md P4.3).
const SLOTS: u32 = 256;
/// Ticks between attach attempts while the driver is missing.
const RETRY_TICKS: u64 = 100;
/// Ticks without any message from the driver (it sends a keep-alive every
/// 100) before the attachment is presumed dead.
const SILENCE_TICKS: u64 = 500;

/// What `Info` said about the card.
#[derive(Clone, Copy)]
pub(super) struct Card {
    pub(super) mac: [u8; 6],
    pub(super) mtu: u32,
    pub(super) link: bool,
}

pub(super) struct Nic {
    client: Option<Client>,
    shared: Option<Shared>,
    pub(super) card: Option<Card>,
    next_try: u64,
    last_heard: u64,
}

impl Nic {
    pub(super) fn new() -> Nic {
        Nic {
            client: None,
            shared: None,
            card: None,
            next_try: 0,
            last_heard: 0,
        }
    }

    pub(super) fn attached(&self) -> bool {
        self.shared.is_some()
    }

    /// Whether it is time to try attaching again.
    pub(super) fn should_try(&self, now: u64) -> bool {
        !self.attached() && now >= self.next_try
    }

    /// The driver sent something (a keep-alive or an event).
    pub(super) fn heard(&mut self, now: u64) {
        self.last_heard = now;
    }

    /// Whether the driver has gone quiet for too long.
    pub(super) fn silent(&self, now: u64) -> bool {
        self.attached() && now.saturating_sub(self.last_heard) > SILENCE_TICKS
    }

    /// Resolve the driver and read the card, without attaching (so the stack
    /// can be built with the right MAC).
    pub(super) fn probe(&mut self) -> Result<Card, String> {
        // Every resolve opens a new handle: give the old one back first.
        if let Some(old) = self.client.take() {
            old.release();
        }
        let client = Client::connect().map_err(|e| format!("no NIC driver: {}", e.message()))?;
        let info = client
            .info()
            .map_err(|e| format!("Info: {}", e.message()))?;
        let mac: [u8; 6] = info
            .mac
            .as_slice()
            .try_into()
            .map_err(|_| String::from("the driver reported a bad MAC"))?;
        let card = Card {
            mac,
            mtu: info.mtu,
            link: info.link,
        };
        self.client = Some(client);
        self.card = Some(card);
        Ok(card)
    }

    /// Attach the rings to `stack`'s device. `published` is the endpoint `netd`
    /// registered, resolved again so a fresh handle can be transferred.
    pub(super) fn attach(&mut self, stack: &mut Stack, now: u64) -> Result<(), String> {
        self.next_try = now + RETRY_TICKS;
        let card = self.probe()?;
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| String::from("no client"))?;
        if card.mac != stack.mac() {
            stack.set_mac(card.mac);
        }
        // A handle to our own service endpoint, for the driver to post into.
        let own = registry::resolve(super::NAME)
            .map_err(|e| format!("resolving ourselves: {}", e.message()))?;
        let attachment = match client.attach_notifying(SLOTS, own) {
            Ok(attachment) => attachment,
            Err(error) => {
                // The handle is ours again if the transfer never happened.
                let _ = own.release();
                return Err(format!("AttachRing: {}", error.message()));
            }
        };
        let (rx, tx, shared) = attachment.split();
        stack.device_mut().attach(rx, tx);
        self.shared = Some(shared);
        self.last_heard = now;
        Ok(())
    }

    /// Drop the rings (the device first, so nothing reads the memory), tell
    /// the driver if it is still there, and release the shared buffer.
    pub(super) fn detach(&mut self, stack: &mut Stack, why: &str) {
        let Some(shared) = self.shared.take() else {
            return;
        };
        stack.device_mut().detach();
        if let Some(client) = self.client.as_ref() {
            let _ = client.detach(shared.ring);
        }
        shared.close();
        sys::write_str(&format!("NETD:NIC:DETACH {why}\n"));
    }

    /// Tell the driver the transmit ring has frames. A dead driver makes the
    /// send fail, which ends the attachment.
    pub(super) fn kick(&mut self, stack: &mut Stack) {
        let Some(shared) = self.shared.as_ref() else {
            return;
        };
        let ring = shared.ring;
        if let Some(client) = self.client.as_ref() {
            if client.kick(ring).is_err() {
                self.detach(stack, "the driver is gone (kick failed)");
            }
        }
    }

    /// Re-read the card for its link state.
    pub(super) fn refresh_link(&mut self) -> Option<bool> {
        let info = self.client.as_ref()?.info().ok()?;
        let card = self.card.as_mut()?;
        card.link = info.link;
        Some(info.link)
    }
}

/// The interface id of `Notify` messages from the driver.
pub(super) const DRIVER_INTERFACE: u64 = nic::INTERFACE;
