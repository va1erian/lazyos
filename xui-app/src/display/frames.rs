//! The pipelined present (issue #372, protocol from #361): buffer slots,
//! one-way `Present`, and the `BufferRelease`/`FrameDone` events that hand a
//! slot back and pace the next frame.
//!
//! A surface that has used `Present` refuses the legacy `AttachBuffer`
//! (`EBUSY`), so an xui window uses these from its first buffer on.

use libmessenger::{Buffer, Parcel};
use messenger_generated::os_lazy_display_v1 as wire;
use xui_core::Rect;

#[cfg(test)]
use super::request;
use super::Client;

/// A compositor event about a presented frame, on the surface's event
/// endpoint.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameEvent {
    /// The compositor no longer reads `slot`; the client may write it again.
    BufferRelease { slot: u32 },
    /// The compositor consumed the present numbered `seq`.
    FrameDone { seq: u64 },
}

/// Decode a frame event, or `None` when `parcel` is not a well-formed one.
pub fn decode_frame_event(parcel: &Parcel) -> Option<FrameEvent> {
    let body = &parcel.body;
    match parcel.header.method {
        wire::METHOD_BUFFERRELEASE => Some(FrameEvent::BufferRelease {
            slot: wire::decode_buffer_release_args(body).ok()?.slot,
        }),
        wire::METHOD_FRAMEDONE => Some(FrameEvent::FrameDone {
            seq: wire::decode_frame_done_args(body).ok()?.seq,
        }),
        _ => None,
    }
}

impl Client {
    /// `AttachBufferSlot`: register `buffer` (a handle from the display
    /// syscall's `create_buffer`, `len` bytes) as slot `slot` of `surface`.
    /// The compositor refuses the slot it is currently reading (`EBUSY`) and a
    /// buffer too small for the surface's current size (`EINVAL`).
    pub fn attach_slot(&self, surface: u64, slot: u32, buffer: u64, len: u64) -> Result<(), i64> {
        Ok(self
            .wire()?
            .attach_buffer_slot(surface, slot, Buffer::whole(buffer, len))?)
    }

    /// `Present` (one-way): make `slot` the surface's current buffer and
    /// composite `damage` (content-relative). The answer arrives later as
    /// [`FrameEvent`]s; nothing here waits for the compositor.
    pub fn present(&self, surface: u64, slot: u32, seq: u64, damage: Rect) -> Result<(), i64> {
        let rect = wire::Rect {
            x: damage.left.max(0) as u32,
            y: damage.top.max(0) as u32,
            w: damage.width().max(0) as u32,
            h: damage.height().max(0) as u32,
        };
        Ok(self.wire()?.present(surface, slot, seq, &[rect])?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_and_frame_done_decode_and_input_does_not() {
        let release = wire::encode_buffer_release_args(&wire::BufferReleaseArgs {
            surface: 2,
            slot: 1,
        })
        .unwrap();
        let parcel = request(wire::METHOD_BUFFERRELEASE, release, Vec::new());
        assert_eq!(
            decode_frame_event(&parcel),
            Some(FrameEvent::BufferRelease { slot: 1 })
        );
        let done =
            wire::encode_frame_done_args(&wire::FrameDoneArgs { surface: 2, seq: 7 }).unwrap();
        let parcel = request(wire::METHOD_FRAMEDONE, done, Vec::new());
        assert_eq!(
            decode_frame_event(&parcel),
            Some(FrameEvent::FrameDone { seq: 7 })
        );
        let key = wire::encode_key_down_args(&wire::KeyDownArgs { key: 3 }).unwrap();
        let parcel = request(wire::METHOD_KEYDOWN, key, Vec::new());
        assert_eq!(decode_frame_event(&parcel), None);
    }
}
