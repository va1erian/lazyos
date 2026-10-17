//! A frame and some rectangles of it, as the C side passes them, made into what the window
//! takes: the pure half of `lzw_ui_frame_delta` (`ffi_paint.rs`).

use crate::damage::{IRect, Scroll};
use crate::mailbox::{Delta, Patch};

/// Rectangles as the C side passes them: `x, y, width, height` quadruples. A trailing partial
/// quadruple is ignored.
pub fn rects_from_ints(ints: &[i32]) -> Vec<IRect> {
    ints.chunks_exact(4)
        .map(|q| IRect::new(q[0], q[1], q[2], q[3]))
        .collect()
}

/// A scroll as the C side passes it: `x, y, width, height, dx, dy`. `None` when nothing moves.
pub fn scroll_from_ints(ints: &[i32; 6]) -> Option<Scroll> {
    let area = IRect::new(ints[0], ints[1], ints[2], ints[3]);
    (!area.is_empty() && (ints[4] != 0 || ints[5] != 0)).then_some(Scroll {
        area,
        dx: ints[4],
        dy: ints[5],
    })
}

/// A delta from a whole frame (`rgba`, `width * height * 4` bytes) and the rectangles of it to
/// send: each rectangle's rows are copied. `None` when a rectangle is empty or not inside the
/// frame (what the window would refuse anyway), or when the scroll's area leaves it.
pub fn delta_from_frame(
    rgba: &[u8],
    width: u32,
    height: u32,
    scroll: Option<Scroll>,
    rects: &[IRect],
) -> Option<Delta> {
    let frame = IRect::new(
        0,
        0,
        i32::try_from(width).ok()?,
        i32::try_from(height).ok()?,
    );
    if rgba.len()
        != (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?
    {
        return None;
    }
    if scroll.is_some_and(|scroll| !frame.contains(&scroll.area)) {
        return None;
    }
    let stride = width as usize * 4;
    let mut patches = Vec::with_capacity(rects.len());
    for rect in rects {
        if rect.is_empty() || !frame.contains(rect) {
            return None;
        }
        let row = rect.width() as usize * 4;
        let mut copy = Vec::with_capacity(row * rect.height() as usize);
        for y in rect.y0..rect.y1 {
            let start = y as usize * stride + rect.x0 as usize * 4;
            copy.extend_from_slice(&rgba[start..start + row]);
        }
        patches.push(Patch {
            rect: *rect,
            rgba: copy,
        });
    }
    Some(Delta {
        size: (width, height),
        scroll,
        patches,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangles_and_scrolls_from_the_c_side() {
        assert_eq!(
            rects_from_ints(&[1, 2, 3, 4, 5, 6, 7, 8, 9]),
            vec![IRect::new(1, 2, 3, 4), IRect::new(5, 6, 7, 8)]
        );
        assert_eq!(rects_from_ints(&[]), vec![]);
        assert_eq!(scroll_from_ints(&[0, 0, 10, 10, 0, 0]), None, "no offset");
        assert_eq!(scroll_from_ints(&[0, 0, 0, 10, 1, 0]), None, "no area");
        assert_eq!(
            scroll_from_ints(&[0, 0, 10, 10, 0, -3]),
            Some(Scroll {
                area: IRect::new(0, 0, 10, 10),
                dx: 0,
                dy: -3
            })
        );
    }

    #[test]
    fn a_delta_copies_the_rows_of_each_rectangle() {
        let (w, h) = (4u32, 3u32);
        let rgba: Vec<u8> = (0..w * h * 4).map(|i| i as u8).collect();
        let delta = delta_from_frame(&rgba, w, h, None, &[IRect::new(1, 1, 2, 2)]).unwrap();
        assert_eq!(delta.size, (4, 3));
        // Row 1 starts at pixel 4 (byte 16), column 1 is pixel 5 (byte 20); two pixels per row.
        assert_eq!(
            delta.patches[0].rgba,
            [rgba[20..28].to_vec(), rgba[36..44].to_vec()].concat()
        );
    }

    #[test]
    fn a_delta_from_a_bad_rectangle_or_frame_is_refused() {
        let rgba = vec![0u8; 4 * 3 * 4];
        let whole = [IRect::new(0, 0, 4, 3)];
        assert!(delta_from_frame(&rgba, 4, 3, None, &whole).is_some());
        assert!(
            delta_from_frame(&rgba, 4, 3, None, &[IRect::new(3, 0, 2, 1)]).is_none(),
            "outside"
        );
        assert!(
            delta_from_frame(&rgba, 4, 3, None, &[IRect::new(0, 0, 0, 1)]).is_none(),
            "empty"
        );
        assert!(
            delta_from_frame(&rgba, 4, 3, None, &[IRect::new(-1, 0, 2, 1)]).is_none(),
            "negative"
        );
        assert!(
            delta_from_frame(&rgba[1..], 4, 3, None, &whole).is_none(),
            "short frame"
        );
        let leaving = Scroll {
            area: IRect::new(0, 0, 5, 3),
            dx: 1,
            dy: 0,
        };
        assert!(
            delta_from_frame(&rgba, 4, 3, Some(leaving), &whole).is_none(),
            "area leaves the frame"
        );
    }
}
