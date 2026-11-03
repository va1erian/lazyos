//! `btd`'s end of the transport `usbd` serves (`os.lazy.usb.bt`): two
//! framing rings and a doorbell. Packets travel as H4 frames (an indicator
//! byte and the HCI packet); everything that comes in is a stranger's, so
//! each frame is copied out and parsed by the host with its lengths checked.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

use btusb::{PACKET_ACL, PACKET_COMMAND, PACKET_EVENT};
use framering::{FrameBuf, PopError, PushError};
use user::messenger::registry;
use user::messenger::usb_bt::{self as transport, wire, Attachment, Client};
use user::sys;

/// Milliseconds per PIT tick.
const TICK_MS: u64 = sys::TICK_NS / 1_000_000;

/// Frames waiting for room in the `Commands` ring; past it the controller is
/// not reading and the link is dropped.
const OUTBOX_LIMIT: usize = 256;

/// Why the link ended.
#[derive(Debug)]
pub(super) enum Failure {
    /// `usbd` withdrew the controller (unplugged).
    Gone,
    /// A ring broke its protocol, or the outbox overflowed.
    Broken(&'static str),
}

/// What one frame from the controller is.
pub(super) enum Frame {
    Event(Vec<u8>),
    Acl(Vec<u8>),
}

pub(super) struct Link {
    client: Client,
    attachment: Attachment,
    frame: Box<FrameBuf>,
    /// Frames the `Commands` ring had no room for.
    outbox: VecDeque<Vec<u8>>,
    /// The registration this link was made to (see [`transport_object`]).
    object: Option<u64>,
}

/// Whether `os.lazy.usb.bt` is registered by `usbd` (`_usb`, or root on a
/// kernel-spawned boot): the radio's packets are trusted to no other server.
pub(super) fn served_by_usbd() -> bool {
    transport_object().is_some()
}

/// The kernel object `usbd` registered `os.lazy.usb.bt` as, if `usbd` serves
/// it. A controller that left and came back is a new object, even when the
/// name was never seen absent.
pub(super) fn transport_object() -> Option<u64> {
    registry::list().ok()?.iter().find_map(|entry| {
        let trusted = entry.owner_uid == u64::from(usbpolicy::USB_UID) || entry.owner_uid == 0;
        (entry.name == transport::NAME && trusted).then_some(entry.object_id)
    })
}

impl Link {
    /// Attach to `usbd`'s controller if it is served, or say why not.
    pub(super) fn open() -> Result<Link, &'static str> {
        if !served_by_usbd() {
            return Err("no controller");
        }
        let object = transport_object();
        let client = Client::connect().map_err(|_| "connect")?;
        match client.attach(transport::SLOTS) {
            Ok(attachment) => Ok(Link {
                client,
                attachment,
                frame: Box::new([0; framering::MAX_FRAME]),
                outbox: VecDeque::new(),
                object,
            }),
            Err(_) => {
                client.release();
                Err("attach")
            }
        }
    }

    /// The registration this link was made to.
    pub(super) fn object(&self) -> Option<u64> {
        self.object
    }

    /// The controller's USB ids from `Describe()`, for the firmware phase;
    /// `None` when `usbd` does not say (the host then loads nothing).
    pub(super) fn usb_ids(&self) -> Option<bthost::UsbIds> {
        let device = self.client.describe().ok()?;
        Some(bthost::UsbIds {
            vendor: u16::try_from(device.vendor).ok()?,
            product: u16::try_from(device.product).ok()?,
        })
    }

    pub(super) fn ring(&self) -> u32 {
        self.attachment.ring
    }

    /// Whether frames wait for room in the `Commands` ring.
    pub(super) fn backlogged(&self) -> bool {
        !self.outbox.is_empty()
    }

    pub(super) fn send_command(&mut self, packet: &[u8]) -> Result<(), Failure> {
        self.send(PACKET_COMMAND, packet)
    }

    pub(super) fn send_acl(&mut self, packet: &[u8]) -> Result<(), Failure> {
        self.send(PACKET_ACL, packet)
    }

    fn send(&mut self, indicator: u8, packet: &[u8]) -> Result<(), Failure> {
        if self.outbox.len() >= OUTBOX_LIMIT {
            return Err(Failure::Broken("the command ring is not being read"));
        }
        let mut frame = Vec::with_capacity(1 + packet.len());
        frame.push(indicator);
        frame.extend_from_slice(packet);
        self.outbox.push_back(frame);
        self.flush()
    }

    /// Push what the ring has room for, then wake `usbd` if it sleeps.
    pub(super) fn flush(&mut self) -> Result<(), Failure> {
        let mut pushed = false;
        while let Some(frame) = self.outbox.front() {
            match self.attachment.commands.push(frame) {
                Ok(()) => {
                    self.outbox.pop_front();
                    pushed = true;
                }
                Err(PushError::Full) => break,
                Err(_) => return Err(Failure::Broken("the command ring is broken")),
            }
        }
        if pushed && self.attachment.commands.take_notify() {
            let _ = self.client.kick(self.attachment.ring);
        }
        Ok(())
    }

    /// The next frame from the controller, copied out. A frame no packet
    /// could be (empty, an unknown indicator) is skipped.
    pub(super) fn next_frame(&mut self) -> Result<Option<Frame>, Failure> {
        loop {
            let len = match self.attachment.events.pop(&mut self.frame) {
                Ok(Some(len)) => len,
                Ok(None) => return Ok(None),
                Err(PopError::BadLength(_)) => continue,
                Err(PopError::Corrupt) => return Err(Failure::Broken("the event ring is broken")),
            };
            let Some((&indicator, packet)) = self.frame[..len].split_first() else {
                continue;
            };
            match indicator {
                PACKET_EVENT => return Ok(Some(Frame::Event(packet.to_vec()))),
                PACKET_ACL => return Ok(Some(Frame::Acl(packet.to_vec()))),
                _ => {}
            }
        }
    }

    /// Sleep until `usbd` has events or the absolute tick `deadline`: arm the
    /// ring, look once more, and only then wait for the doorbell.
    pub(super) fn sleep(&mut self, deadline: u64) -> Result<(), Failure> {
        self.attachment.events.arm();
        if matches!(self.attachment.events.pending(), Ok(n) if n > 0) {
            return Ok(());
        }
        match self.attachment.wait(deadline) {
            Ok(Some(bits)) if bits & (1 << wire::NOTIFY_BIT_GONE) != 0 => Err(Failure::Gone),
            Ok(_) => Ok(()),
            Err(_) => Err(Failure::Gone),
        }
    }

    pub(super) fn close(self) {
        let _ = self.client.detach(self.attachment.ring);
        self.attachment.close();
        self.client.release();
    }
}

/// The absolute PIT tick for a host deadline in milliseconds, at least one
/// tick ahead.
pub(super) fn deadline_ticks(now_ms: u64, deadline_ms: u64) -> u64 {
    let wait_ms = deadline_ms.saturating_sub(now_ms);
    sys::clock().saturating_add(wait_ms.div_ceil(TICK_MS).max(1))
}
