//! The full interface's `Frame` arguments: what a client reads to apply a slot (a whole frame, or a
//! delta's shift and rectangles) and the scroll offset its pixels were painted at.

use messenger_generated::os_lazy_webd_view_v1 as wire;

use crate::events::pictures::{DeltaNow, Offset};

/// The full interface's `Frame` arguments for a frame of `width` x `height` in `slot`.
pub(super) fn frame_args(
    view: u64,
    slot: usize,
    seq: u64,
    (width, height): (u32, u32),
    delta: Option<&DeltaNow>,
    offset: Option<Offset>,
) -> wire::FrameArgs {
    let (offset_x, offset_y, at_bottom) = offset.unwrap_or((0, 0, false));
    wire::FrameArgs {
        view,
        slot: slot as u32,
        seq,
        width,
        height,
        whole: delta.is_none(),
        shift: delta
            .and_then(|d| d.shift)
            .map(|[x, y, width, height, dx, dy]| wire::Shift {
                x,
                y,
                width,
                height,
                dx,
                dy,
            }),
        rects: delta
            .map(|d| {
                d.patches
                    .iter()
                    .map(|p| wire::Rect {
                        x: p.rect[0],
                        y: p.rect[1],
                        width: p.rect[2],
                        height: p.rect[3],
                    })
                    .collect()
            })
            .unwrap_or_default(),
        offset_x,
        offset_y,
        at_bottom,
    }
}
