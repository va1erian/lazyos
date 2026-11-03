//! The Bluetooth function's side of its rings (`idl/usb_bt.midl`): what
//! `btd` attached and the bounded queue behind a full ring.
//!
//! `btd` creates one shared buffer holding the `Commands` ring (it produces,
//! `usbd` consumes) and the `Events` ring (`usbd` produces) and hands it over
//! with a notify endpoint. Both rings treat the peer as hostile
//! (`libs/framering`): an impossible index poisons the ring and the
//! attachment is dropped. Wake-ups follow the NIC rule: `usbd` sends `Notify`
//! only when `btd` armed the `Events` ring, and arms the `Commands` ring
//! itself before it parks, so `Kick` is sent only to a sleeping `usbd`.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use user::trace;

use btusb::PACKET_ACL;
use framering::{ring_bytes, valid_slots, Consumer, FrameBuf, Producer, PushError, Ring};
use user::messenger::usb_bt::{self as api, wire};
use user::messenger::{errno, Endpoint, Error as MsgError};
use user::sys;

/// Frames that wait for room in a full `Events` ring. Past it the oldest
/// packets are what the host stack already lost; the newest are dropped and
/// counted (`Stats.dropped`).
pub(super) const BACKLOG: usize = 64;

pub(super) struct Link {
    /// The kernel-stamped sender of `AttachRing`: the only caller of the
    /// owner methods.
    pub(super) owner: u64,
    pub(super) ring: u32,
    buffer: u64,
    notify: Endpoint,
    events: Producer,
    commands: Consumer,
    /// The `Commands` ring was full when last looked at.
    was_full: bool,
    /// `btd` has frames waiting for room and asked (with a `Kick`) to be
    /// told when there is some: the ring may have filled after the last look.
    space_wanted: bool,
}

/// The state of the `Events` ring after a push.
pub(super) enum Pushed {
    Queued,
    /// No room: the caller keeps the frame in its backlog.
    Full,
    /// The peer broke the ring protocol: the attachment must go.
    Poisoned,
}

fn invalid() -> MsgError {
    MsgError::Errno(-errno::EINVAL)
}

impl Link {
    /// Map the client's buffer and take its two rings. The caller closes the
    /// buffer and releases `notify` when this fails.
    pub(super) fn attach(
        owner: u64,
        ring: u32,
        args: &wire::AttachRingArgs,
        notify: Endpoint,
    ) -> Result<Link, MsgError> {
        if !valid_slots(args.slots) {
            return Err(invalid());
        }
        let one = ring_bytes(args.slots) as u64;
        let layout = wire::attach_ring_rings(one).ok_or_else(invalid)?;
        // The buffer must be exactly the two rings: no slack for a peer to
        // hide a third thing in.
        if args.rings.len != layout.total {
            return Err(invalid());
        }
        let (va, size) = sys::buffer_map(args.rings.handle).map_err(MsgError::Errno)?;
        // The range is the client's claim; the mapped size is the truth.
        if !args.rings.fits(size) {
            return Err(invalid());
        }
        let base = usize::try_from(args.rings.offset)
            .ok()
            .and_then(|offset| (va as usize).checked_add(offset))
            .ok_or_else(invalid)?;
        let (commands_at, events_at) = (layout.commands as usize, layout.events as usize);
        let one = one as usize;
        // SAFETY: `offset..offset + total` lies inside the `size` bytes the
        // kernel mapped at `va` (checked above), and each ring is `one` bytes
        // at an offset the layout keeps inside `total`. The mapping stays
        // until `Link::close`, which runs only after the endpoints below are
        // dropped with the link. The client may rewrite the memory at any
        // time, which the ring implementation is built to tolerate.
        let (commands, events) = unsafe {
            (
                Ring::attach((base + commands_at) as *mut u8, one, args.slots),
                Ring::attach((base + events_at) as *mut u8, one, args.slots),
            )
        };
        let (Ok(commands), Ok(events)) = (commands, events) else {
            return Err(invalid());
        };
        Ok(Link {
            owner,
            ring,
            buffer: args.rings.handle,
            notify,
            events: events.producer(),
            commands: commands.consumer(),
            was_full: false,
            space_wanted: false,
        })
    }

    /// Frames waiting in the `Commands` ring, or `None` when it is poisoned.
    pub(super) fn pending(&mut self) -> Option<u32> {
        let pending = self.commands.pending().ok()?;
        self.was_full = pending >= self.commands.slots();
        Some(pending)
    }

    /// Take the next command frame into `out`; `Ok(None)` when the ring is
    /// empty, `Err` when it is poisoned. A slot with an impossible length is
    /// skipped (`Some(0)`: the caller counts it as refused).
    pub(super) fn pop(&mut self, out: &mut FrameBuf) -> Result<Option<usize>, ()> {
        match self.commands.pop(out) {
            Ok(frame) => Ok(frame),
            Err(framering::PopError::BadLength(_)) => Ok(Some(0)),
            Err(framering::PopError::Corrupt) => Err(()),
        }
    }

    /// Ask for a `Kick` the next time `btd` queues a command, and look once
    /// more: `true` when a frame slipped in, so the caller must not sleep.
    pub(super) fn arm_commands(&mut self) -> bool {
        self.commands.arm();
        matches!(self.commands.pending(), Ok(n) if n > 0)
    }

    /// `btd` kicked: it has a backlog and waits for room.
    pub(super) fn want_space(&mut self) {
        self.space_wanted = true;
    }

    /// Whether the `Commands` ring was full (or `btd` said it is waiting) and
    /// has room now: `btd` must be told.
    pub(super) fn took_space(&mut self) -> bool {
        core::mem::take(&mut self.was_full) | core::mem::take(&mut self.space_wanted)
    }

    pub(super) fn push(&mut self, frame: &[u8]) -> Pushed {
        match self.events.push(frame) {
            Ok(()) => Pushed::Queued,
            Err(PushError::Full) => Pushed::Full,
            Err(PushError::Corrupt) => Pushed::Poisoned,
            // `frame` is `usbd`'s own, built to fit a slot.
            Err(PushError::Empty | PushError::TooLong) => Pushed::Queued,
        }
    }

    /// After a burst of pushes: whether `btd` armed the ring and so wants a
    /// `Notify`.
    pub(super) fn wants_notify(&mut self) -> bool {
        self.events.take_notify()
    }

    /// Send `btd` a `Notify` with `bits` (`NotifyBit` positions as a mask).
    /// `false` when the endpoint is dead: `btd` is gone.
    pub(super) fn notify(&self, bits: u32) -> bool {
        let args = wire::NotifyArgs {
            ring: self.ring,
            events: bits,
        };
        let Ok(body) = wire::encode_notify_args(&args) else {
            return true;
        };
        match self.notify.send(&api::event(wire::METHOD_NOTIFY, body)) {
            Ok(()) => true,
            // A full inbox only loses this (coalesced) notice.
            Err(error) => {
                !matches!(error.errno(), Some(code) if code == -errno::EPIPE || code == -errno::EBADF)
            }
        }
    }

    /// Unmap the buffer and give the notify endpoint back. The ring
    /// endpoints go with `self`.
    pub(super) fn close(self, why: &str) {
        trace::line!(
            trace::Channel::Usbd,
            "BT:DETACH owner={} ring={} {why}",
            self.owner,
            self.ring
        );
        let _ = sys::buffer_close(self.buffer);
        let _ = self.notify.release();
    }
}

/// Events waiting for room in the `Events` ring, oldest first.
#[derive(Default)]
pub(super) struct Backlog {
    frames: VecDeque<Vec<u8>>,
}

impl Backlog {
    pub(super) fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Keep `frame`; `false` when a frame was lost for it: the backlog was
    /// full and the frame was dropped, or (`event`) it took the place of the
    /// oldest ACL frame, which was.
    pub(super) fn push(&mut self, frame: &[u8], event: bool) -> bool {
        if self.frames.len() >= BACKLOG {
            let oldest_acl = self
                .frames
                .iter()
                .position(|queued| queued.first() == Some(&PACKET_ACL));
            match oldest_acl {
                Some(at) if event => {
                    self.frames.remove(at);
                    self.frames.push_back(frame.to_vec());
                }
                _ => {}
            }
            return false;
        }
        self.frames.push_back(frame.to_vec());
        true
    }

    /// Move what fits into the ring; whether anything moved, or the ring
    /// turned out to be poisoned (`Err`).
    pub(super) fn drain_into(&mut self, link: &mut Link) -> Result<bool, ()> {
        let mut moved = false;
        while let Some(frame) = self.frames.front() {
            match link.push(frame) {
                Pushed::Queued => {
                    self.frames.pop_front();
                    moved = true;
                }
                Pushed::Full => break,
                Pushed::Poisoned => return Err(()),
            }
        }
        Ok(moved)
    }

    pub(super) fn clear(&mut self) {
        self.frames.clear();
    }
}
