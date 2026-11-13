//! The pointer half of `inputd` (`docs/usb-hid-plan.md`, phase P1): the
//! cursor every pointing device moves, delivered to the compositor only.
//!
//! The decisions live in `inputmap::Pointer`; this module answers the shell's
//! pointer calls and turns outputs into `PointerEvent`s. A compositor gets
//! pointer events only once it has asked about the pointer (`SetBounds` or
//! `GetPointer`), so one that still takes its pointer from the kernel's display
//! stream is never flooded with events it ignores.

use alloc::vec::Vec;

use inputmap::pointer::MAX_SIDE;
use inputmap::{Pointer, PointerOut};
use libmessenger::client::Errno;
use libmessenger::server::Fault;
use user::messenger::input::shell_wire;
use user::sys;

use super::hub::Hub;

/// The kernel's default cursor bounds, used until the compositor sets them.
const DEFAULT_BOUNDS: (u32, u32) = (1280, 720);

pub(super) struct Cursor {
    pub(super) engine: Pointer,
    /// The attached compositor asked for pointer events.
    pub(super) subscribed: bool,
    /// The buttons held in the last event sent, to spot a press.
    pub(super) sent_buttons: u32,
}

impl Cursor {
    pub(super) fn new() -> Cursor {
        Cursor {
            engine: Pointer::new(DEFAULT_BOUNDS.0, DEFAULT_BOUNDS.1),
            subscribed: false,
            sent_buttons: 0,
        }
    }
}

impl Hub {
    /// `SetBounds`, from the attached compositor (the caller checked the
    /// sender). Only a call that succeeds subscribes it.
    pub(super) fn set_pointer_bounds(&mut self, width: u32, height: u32) -> Result<(), Fault> {
        let valid = 1..=MAX_SIDE;
        if !valid.contains(&width) || !valid.contains(&height) {
            return Err(Fault::errno(Errno::INVAL));
        }
        self.pointer.subscribed = true;
        if self.pointer.engine.set_bounds(width, height) {
            let mut outputs = Vec::new();
            self.pointer.engine.flush(&mut outputs);
            self.deliver_pointer(&outputs);
        }
        Ok(())
    }

    /// `GetPointer`, the same.
    pub(super) fn pointer_state(&mut self) -> shell_wire::GetPointerReply {
        self.pointer.subscribed = true;
        let (x, y) = self.pointer.engine.position();
        shell_wire::GetPointerReply {
            x,
            y,
            buttons: self.pointer.engine.buttons(),
        }
    }

    /// Send pointer outputs to the compositor, if it subscribed.
    pub(super) fn deliver_pointer(&mut self, outputs: &[PointerOut]) {
        if !self.pointer.subscribed {
            return;
        }
        let now = sys::clock();
        for out in outputs {
            self.note_pointer_sent(out, now);
            let body = shell_wire::encode_pointer_event_args(&shell_wire::PointerEventArgs {
                x: out.x,
                y: out.y,
                buttons: out.buttons,
                wheel: out.wheel_v,
                wheel_h: out.wheel_h,
                ts_ns: out.ts_ns,
                seq: out.seq,
            });
            self.shell_event(shell_wire::METHOD_POINTEREVENT, body);
        }
    }
}
