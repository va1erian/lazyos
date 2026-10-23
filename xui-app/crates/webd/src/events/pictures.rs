//! The pictures of a view with deltas, between WebKit's thread and the service's: the whole frame
//! and the deltas posted since the service last looked, in order, with the scroll offset each was
//! painted at (the window's mailbox, `xui_webkit::mailbox`, per view).
//!
//! A delta only means something on top of the picture the client has. So the queue keeps a `base`:
//! the size of the last whole frame, and whether the client can still apply deltas to it. A delta
//! without a base, of another size, or one too many is refused (`frame_delta` returns `true`) and the
//! engine paints a whole frame next. A delta that arrives while a whole frame is still waiting is
//! applied to that frame here, so the service sends one picture, not a frame and a chain.

use super::spares::Spares;
use super::FrameNow;
use crate::limits::DELTAS_KEPT;

/// `(x, y, at_bottom)`: the page's scroll offset the pixels were painted at.
pub type Offset = (i32, i32, bool);

/// One repainted rectangle: `[x, y, width, height]` and its RGBA pixels, `width * 4` per row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Patch {
    pub rect: [i32; 4],
    pub rgba: Vec<u8>,
}

/// A change to the picture the client has: move `shift` (`[x, y, width, height, dx, dy]`) first,
/// then lay the patches over it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeltaNow {
    pub width: i32,
    pub height: i32,
    pub shift: Option<[i32; 6]>,
    pub patches: Vec<Patch>,
    pub offset: Option<Offset>,
}

/// What the service collects for a view: a whole frame (if one was posted) and the deltas after it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Update {
    pub whole: Option<(FrameNow, Option<Offset>)>,
    pub deltas: Vec<DeltaNow>,
}

/// One view's pictures in flight.
#[derive(Debug, Default)]
pub struct Pictures {
    update: Update,
    bytes: usize,
    /// The size of the picture deltas apply to; `None` until a whole frame and after a refusal.
    base: Option<(i32, i32)>,
    /// The offset named for the next post (`webd_ui_scroll` comes just before it).
    staged: Option<Offset>,
}

/// Whether `rect` lies inside a whole `width` x `height` RGBA frame held in `rgba`.
pub fn fits(rgba: &[u8], width: i32, height: i32, rect: [i32; 4]) -> bool {
    let [x, y, w, h] = rect.map(i64::from);
    let (width, height) = (i64::from(width), i64::from(height));
    let inside = x >= 0 && y >= 0 && w >= 0 && h >= 0 && x + w <= width && y + h <= height;
    let bytes = (width.max(0) as u64)
        .checked_mul(height.max(0) as u64)
        .and_then(|pixels| pixels.checked_mul(4));
    inside && bytes.is_some_and(|bytes| rgba.len() as u64 >= bytes)
}

/// Copy the `rect` of a whole `width`-wide RGBA frame out, row by row; `None` if it does not fit
/// (the queue itself cuts into reused buffers, `Spares::cut`).
pub fn cut(rgba: &[u8], width: i32, height: i32, rect: [i32; 4]) -> Option<Vec<u8>> {
    fits(rgba, width, height, rect).then(|| Spares::default().cut(rgba, width as usize * 4, rect))
}

/// Move the part of `frame` inside `shift`'s area by its offset, as a client does: what leaves the
/// area is gone, what it uncovers keeps its old pixels.
pub fn apply_shift(frame: &mut FrameNow, shift: [i32; 6]) {
    let [sx, sy, sw, sh, dx, dy] = shift;
    let (w, h) = (frame.width, frame.height);
    let (x0, y0, x1, y1) = (sx.max(0), sy.max(0), (sx + sw).min(w), (sy + sh).min(h));
    // The destination rows and columns that stay inside the area.
    let (tx0, ty0, tx1, ty1) = (
        (x0 + dx).max(x0),
        (y0 + dy).max(y0),
        (x1 + dx).min(x1),
        (y1 + dy).min(y1),
    );
    if tx1 <= tx0 || ty1 <= ty0 {
        return;
    }
    let stride = w as usize * 4;
    let row_bytes = (tx1 - tx0) as usize * 4;
    let rows = ty1 - ty0;
    for i in 0..rows {
        let row = if dy > 0 { rows - 1 - i } else { i };
        let to = (ty0 + row) as usize * stride + tx0 as usize * 4;
        let from = (ty0 + row - dy) as usize * stride + (tx0 - dx) as usize * 4;
        frame.rgba.copy_within(from..from + row_bytes, to);
    }
}

/// Lay `patch` over `frame`.
pub fn apply_patch(frame: &mut FrameNow, patch: &Patch) {
    let [x, y, w, _] = patch.rect;
    let stride = frame.width as usize * 4;
    for (row, line) in patch.rgba.chunks_exact(w.max(1) as usize * 4).enumerate() {
        let start = (y as usize + row) * stride + x as usize * 4;
        frame.rgba[start..start + line.len()].copy_from_slice(line);
    }
}

/// Copy `rect` of the whole frame `rgba` (as wide as `frame`) over the same place in `frame`.
fn copy_rect(frame: &mut FrameNow, rgba: &[u8], rect: [i32; 4]) {
    let [x, y, w, h] = rect.map(|v| v.max(0) as usize);
    let stride = frame.width as usize * 4;
    for row in y..y + h {
        let start = row * stride + x * 4;
        frame.rgba[start..start + w * 4].copy_from_slice(&rgba[start..start + w * 4]);
    }
}

/// The pixels of deltas that will not be sent, back for reuse.
fn give_deltas(spares: &mut Spares, deltas: Vec<DeltaNow>) {
    for patch in deltas.into_iter().flat_map(|delta| delta.patches) {
        spares.keep(patch.rgba);
    }
}

impl Pictures {
    pub fn stage_offset(&mut self, offset: Offset) {
        self.staged = Some(offset);
    }

    /// A whole frame: it replaces everything waiting and gives deltas a base again.
    pub fn post_whole(&mut self, spares: &mut Spares, rgba: &[u8], width: i32, height: i32) {
        let offset = self.staged.take();
        // The deltas this frame supersedes go back first: their buffers may hold it.
        give_deltas(spares, std::mem::take(&mut self.update.deltas));
        let mut buffer = match self.update.whole.take() {
            Some((frame, _)) => frame.rgba,
            None => spares.take(rgba.len()),
        };
        buffer.clear();
        buffer.extend_from_slice(rgba);
        let frame = FrameNow {
            width,
            height,
            rgba: buffer,
        };
        self.update.whole = Some((frame, offset));
        self.bytes = 0;
        self.base = Some((width, height));
    }

    /// A delta: `true` when it is refused (the engine paints a whole frame next).
    pub fn post_delta(
        &mut self,
        spares: &mut Spares,
        rgba: &[u8],
        width: i32,
        height: i32,
        shift: Option<[i32; 6]>,
        rects: &[[i32; 4]],
    ) -> bool {
        let offset = self.staged.take();
        if self.base != Some((width, height))
            || !rects.iter().all(|rect| fits(rgba, width, height, *rect))
        {
            self.base = None;
            return true;
        }
        if let Some((frame, whole_offset)) = &mut self.update.whole {
            // Still waiting: the delta is applied to it, and it is still one whole picture.
            if let Some(shift) = shift {
                apply_shift(frame, shift);
            }
            for rect in rects {
                copy_rect(frame, rgba, *rect);
            }
            if offset.is_some() {
                *whole_offset = offset;
            }
            return false;
        }
        let bytes: usize = rects
            .iter()
            .map(|[_, _, w, h]| (*w as usize) * (*h as usize) * 4)
            .sum();
        let frame_bytes = width as usize * height as usize * 4;
        if self.update.deltas.len() >= DELTAS_KEPT || self.bytes + bytes > frame_bytes {
            give_deltas(spares, std::mem::take(&mut self.update.deltas));
            self.bytes = 0;
            self.base = None;
            return true;
        }
        let stride = width as usize * 4;
        let patches = rects
            .iter()
            .map(|rect| Patch {
                rect: *rect,
                rgba: spares.cut(rgba, stride, *rect),
            })
            .collect();
        self.bytes += bytes;
        self.update.deltas.push(DeltaNow {
            width,
            height,
            shift,
            patches,
            offset,
        });
        false
    }

    /// The client lost a picture (a delta it never got): deltas are refused until a whole frame.
    pub fn lose_base(&mut self, spares: &mut Spares) {
        self.base = None;
        give_deltas(spares, std::mem::take(&mut self.update.deltas));
        self.bytes = 0;
    }

    /// What is waiting, taken.
    pub fn take(&mut self) -> Update {
        self.bytes = 0;
        std::mem::take(&mut self.update)
    }

    pub fn is_empty(&self) -> bool {
        self.update.whole.is_none() && self.update.deltas.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: i32, h: i32, fill: impl Fn(i32, i32) -> u8) -> Vec<u8> {
        let mut out = Vec::new();
        for y in 0..h {
            for x in 0..w {
                out.extend_from_slice(&[fill(x, y); 4]);
            }
        }
        out
    }

    #[test]
    fn a_delta_needs_a_whole_frame_of_its_size_first() {
        let mut p = Pictures::default();
        let mut s = Spares::default();
        let rgba = frame(4, 4, |_, _| 1);
        assert!(p.post_delta(&mut s, &rgba, 4, 4, None, &[[0, 0, 1, 1]]));
        p.post_whole(&mut s, &rgba, 4, 4);
        p.take();
        assert!(!p.post_delta(&mut s, &rgba, 4, 4, None, &[[0, 0, 1, 1]]));
        assert!(p.post_delta(&mut s, &rgba, 5, 4, None, &[]), "another size");
        assert!(
            p.post_delta(&mut s, &rgba, 4, 4, None, &[]),
            "refused until a whole frame"
        );
    }

    #[test]
    fn a_rectangle_outside_the_frame_is_refused() {
        let mut p = Pictures::default();
        let mut s = Spares::default();
        let rgba = frame(4, 4, |_, _| 1);
        p.post_whole(&mut s, &rgba, 4, 4);
        p.take();
        assert!(p.post_delta(&mut s, &rgba, 4, 4, None, &[[3, 3, 2, 1]]));
        assert!(
            p.post_delta(&mut s, &rgba, 4, 4, None, &[[0, 0, 1, 1]]),
            "the base is gone"
        );
    }

    #[test]
    fn deltas_queue_in_order_with_their_offsets_and_are_bounded() {
        let mut p = Pictures::default();
        let mut s = Spares::default();
        let rgba = frame(4, 4, |x, y| (x + 4 * y) as u8);
        p.post_whole(&mut s, &rgba, 4, 4);
        p.take();
        p.stage_offset((0, 7, false));
        assert!(!p.post_delta(
            &mut s,
            &rgba,
            4,
            4,
            Some([0, 0, 4, 4, 0, -1]),
            &[[0, 3, 4, 1]]
        ));
        let update = p.take();
        assert!(update.whole.is_none());
        assert_eq!(update.deltas.len(), 1);
        assert_eq!(update.deltas[0].offset, Some((0, 7, false)));
        assert_eq!(update.deltas[0].patches[0].rgba, rgba[48..64].to_vec());
        for _ in 0..DELTAS_KEPT {
            assert!(!p.post_delta(&mut s, &rgba, 4, 4, None, &[[0, 0, 1, 1]]));
        }
        assert!(
            p.post_delta(&mut s, &rgba, 4, 4, None, &[[0, 0, 1, 1]]),
            "too far behind"
        );
        assert!(p.take().deltas.is_empty());
    }

    #[test]
    fn a_delta_on_a_waiting_whole_frame_is_applied_to_it() {
        let mut p = Pictures::default();
        let mut s = Spares::default();
        let old = frame(4, 4, |_, y| y as u8);
        p.post_whole(&mut s, &old, 4, 4);
        // The page scrolled up by one row: rows move up, the last row is repainted.
        let new = frame(4, 4, |_, y| y as u8 + 1);
        assert!(!p.post_delta(
            &mut s,
            &new,
            4,
            4,
            Some([0, 0, 4, 4, 0, -1]),
            &[[0, 3, 4, 1]]
        ));
        let update = p.take();
        assert!(update.deltas.is_empty());
        assert_eq!(update.whole.unwrap().0.rgba, new);
    }

    #[test]
    fn shifting_down_and_up_moves_rows_without_tearing() {
        for dy in [-2, -1, 1, 2] {
            let mut f = FrameNow {
                width: 3,
                height: 5,
                rgba: frame(3, 5, |_, y| y as u8),
            };
            apply_shift(&mut f, [0, 0, 3, 5, 0, dy]);
            for y in 0..5 {
                let src = y - dy;
                let want = if (0..5).contains(&src) { src } else { y } as u8;
                assert_eq!(f.rgba[(y as usize * 3) * 4], want, "dy {dy} row {y}");
            }
        }
    }
}
