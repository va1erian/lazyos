//! Frames from webd's slots into the window's mailbox: a whole frame is copied once, straight from
//! the slot into the mailbox's buffer; a delta is its moved area and its rectangles, read out of the
//! slot. The scroll offset the pixels were painted at goes with them, as the in-process engine
//! named it before every post.

use std::sync::Arc;

use xui_webd_client::full::{FrameInfo, PixelSink, SlotPixels};
use xui_webkit::damage::{IRect, Scroll};
use xui_webkit::mailbox::{Delta, Mailbox, Patch};

pub(crate) struct MailboxSink<'a> {
    mailbox: &'a Arc<Mailbox>,
}

impl<'a> MailboxSink<'a> {
    pub(crate) fn new(mailbox: &'a Arc<Mailbox>) -> MailboxSink<'a> {
        MailboxSink { mailbox }
    }
}

/// The rectangle `(x, y, width, height)` as the seam's.
fn irect(x: i32, y: i32, width: i32, height: i32) -> IRect {
    IRect {
        x0: x,
        y0: y,
        x1: x.saturating_add(width),
        y1: y.saturating_add(height),
    }
}

impl PixelSink for MailboxSink<'_> {
    fn whole(&mut self, frame: &FrameInfo, slot: &dyn SlotPixels) {
        let (_, y, at_bottom) = frame.offset;
        self.mailbox.set_scroll_offset(y, at_bottom);
        self.mailbox
            .post_frame_with(frame.width, frame.height, |pixels| slot.read_at(0, pixels));
    }

    fn delta(&mut self, frame: &FrameInfo, slot: &dyn SlotPixels) -> bool {
        let stride = frame.width as usize * 4;
        let patches = frame
            .rects
            .iter()
            .map(|r| {
                let row_bytes = r.width.max(0) as usize * 4;
                let mut rgba = vec![0; row_bytes * r.height.max(0) as usize];
                for (row, out) in rgba.chunks_exact_mut(row_bytes.max(1)).enumerate() {
                    let at = (r.y as usize + row) * stride + r.x as usize * 4;
                    slot.read_at(at, out);
                }
                Patch {
                    rect: irect(r.x, r.y, r.width, r.height),
                    rgba,
                }
            })
            .collect();
        let scroll = frame.shift.as_ref().map(|s| Scroll {
            area: irect(s.x, s.y, s.width, s.height),
            dx: s.dx,
            dy: s.dy,
        });
        let (_, y, at_bottom) = frame.offset;
        self.mailbox.set_scroll_offset(y, at_bottom);
        // The window refuses a delta it has no picture for (or is too far behind): webd is asked
        // for a whole frame instead.
        !self.mailbox.post_delta(Delta {
            size: (frame.width, frame.height),
            scroll,
            patches,
        })
    }
}
