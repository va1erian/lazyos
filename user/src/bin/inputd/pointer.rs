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
use user::messenger::input::shell_wire;
use user::messenger::{errno, Error, Result};

use super::hub::Hub;

/// The kernel's default cursor bounds, used until the compositor sets them.
const DEFAULT_BOUNDS: (u32, u32) = (1280, 720);

pub(super) struct Cursor {
    pub(super) engine: Pointer,
    /// The attached compositor asked for pointer events.
    pub(super) subscribed: bool,
}

impl Cursor {
    pub(super) fn new() -> Cursor {
        Cursor {
            engine: Pointer::new(DEFAULT_BOUNDS.0, DEFAULT_BOUNDS.1),
            subscribed: false,
        }
    }
}

impl Hub {
    /// `SetBounds` and `GetPointer`, from the attached compositor (the caller
    /// checked the sender).
    pub(super) fn pointer_call(&mut self, method: u32, body: &[u8]) -> Result<Vec<u8>> {
        self.pointer.subscribed = true;
        match method {
            shell_wire::METHOD_SETBOUNDS => {
                let args = shell_wire::decode_set_bounds_args(body).map_err(Error::Parcel)?;
                let valid = 1..=MAX_SIDE;
                if !valid.contains(&args.width) || !valid.contains(&args.height) {
                    return Err(Error::Errno(-errno::EINVAL));
                }
                if self.pointer.engine.set_bounds(args.width, args.height) {
                    let mut outputs = Vec::new();
                    self.pointer.engine.flush(&mut outputs);
                    self.deliver_pointer(&outputs);
                }
                Ok(Vec::new())
            }
            shell_wire::METHOD_GETPOINTER => {
                let (x, y) = self.pointer.engine.position();
                shell_wire::encode_get_pointer_reply(&shell_wire::GetPointerReply {
                    x,
                    y,
                    buttons: self.pointer.engine.buttons(),
                })
                .map_err(Error::Parcel)
            }
            _ => Err(Error::Errno(-errno::EINVAL)),
        }
    }

    /// Send pointer outputs to the compositor, if it subscribed.
    pub(super) fn deliver_pointer(&mut self, outputs: &[PointerOut]) {
        if !self.pointer.subscribed {
            return;
        }
        for out in outputs {
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
