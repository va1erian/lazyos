//! Rectangle arithmetic and rectangle copies between top-down RGBA images of
//! one size: what the slots and the compositing paths share.

use xui_core::Rect;

/// The smallest rectangle covering `a` and `b`.
pub fn union(a: Rect, b: Rect) -> Rect {
    Rect::new(
        a.left.min(b.left),
        a.top.min(b.top),
        a.right.max(b.right),
        a.bottom.max(b.bottom),
    )
}

/// The pixels `a` and `b` share, or `None` when they share none.
pub fn intersection(a: Rect, b: Rect) -> Option<Rect> {
    let r = Rect::new(
        a.left.max(b.left),
        a.top.max(b.top),
        a.right.min(b.right),
        a.bottom.min(b.bottom),
    );
    (r.left < r.right && r.top < r.bottom).then_some(r)
}

/// `rect` minus `hole`: at most four rectangles (above, below, left, right of
/// the hole), none for an empty `rect`.
pub fn subtract(rect: Rect, hole: Rect) -> Vec<Rect> {
    if rect.left >= rect.right || rect.top >= rect.bottom {
        return Vec::new();
    }
    let Some(cut) = intersection(rect, hole) else {
        return vec![rect];
    };
    let mut out = Vec::with_capacity(4);
    if rect.top < cut.top {
        out.push(Rect::new(rect.left, rect.top, rect.right, cut.top));
    }
    if cut.bottom < rect.bottom {
        out.push(Rect::new(rect.left, cut.bottom, rect.right, rect.bottom));
    }
    if rect.left < cut.left {
        out.push(Rect::new(rect.left, cut.top, cut.left, cut.bottom));
    }
    if cut.right < rect.right {
        out.push(Rect::new(cut.right, cut.top, rect.right, cut.bottom));
    }
    out
}

/// Copy `rect` (clipped to the image) of the `width` x `height` RGBA image
/// `src` into the same place in `dst`: one copy for full-width rows, else one
/// per row. Nothing is copied unless both slices are exactly that long.
pub fn copy_rect(dst: &mut [u8], src: &[u8], width: i32, height: i32, rect: Rect) {
    let stride = width.max(0) as usize * 4;
    let len = stride * height.max(0) as usize;
    if dst.len() != len || src.len() != len {
        return;
    }
    let left = rect.left.clamp(0, width) as usize;
    let right = rect.right.clamp(0, width) as usize;
    let top = rect.top.clamp(0, height) as usize;
    let bottom = rect.bottom.clamp(0, height) as usize;
    if left >= right || top >= bottom {
        return;
    }
    if right - left == width as usize {
        let span = top * stride..bottom * stride;
        dst[span.clone()].copy_from_slice(&src[span]);
        return;
    }
    for row in top..bottom {
        let span = row * stride + left * 4..row * stride + right * 4;
        dst[span.clone()].copy_from_slice(&src[span]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtract_leaves_the_frame_around_a_hole() {
        let rect = Rect::new(0, 0, 100, 100);
        let parts = subtract(rect, Rect::new(10, 20, 90, 80));
        assert_eq!(
            parts,
            vec![
                Rect::new(0, 0, 100, 20),
                Rect::new(0, 80, 100, 100),
                Rect::new(0, 20, 10, 80),
                Rect::new(90, 20, 100, 80)
            ]
        );
        assert!(subtract(rect, rect).is_empty());
        assert_eq!(subtract(rect, Rect::new(200, 0, 300, 10)), vec![rect]);
        assert!(subtract(Rect::new(5, 5, 5, 9), Rect::new(0, 0, 1, 1)).is_empty());
    }

    #[test]
    fn a_scroll_strip_leaves_one_band() {
        // A page scrolled by 40 rows damages the strip it uncovered; the rest
        // of the last frame's damage comes from the slot presented last.
        let stale = Rect::new(0, 0, 100, 60);
        let damage = Rect::new(0, 0, 100, 40);
        assert_eq!(subtract(stale, damage), vec![Rect::new(0, 40, 100, 60)]);
    }
}
