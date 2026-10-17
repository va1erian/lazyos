//! The picture on show, kept as a stack of images instead of one.
//!
//! xui draws an [`Image`] it has never seen by premultiplying and scanning every pixel of it, and
//! an image never changes: so showing each frame as a new image costs a full pass over the page
//! however little of it changed (about 10 ms for a 2000x900 window on a fast PC, one core).
//! Instead the picture is the whole first frame plus the rectangles painted since, each an image
//! with an offset and the area of the view it is visible in. A scroll does not touch the images:
//! it moves their offsets and clips ([`Parts::apply`]), so the pixels xui has already prepared
//! are drawn again and only the rows the scroll exposed are new.
//!
//! The parts are drawn oldest first; a later one covers an earlier one where they meet. Every
//! step keeps that order true to the picture: what [`Parts::render_into`] would paint after a
//! step is what a single image of the same frame would show.

use std::rc::Rc;

use xui_core::Image;

use crate::damage::{IRect, Scroll};
use crate::mailbox::Delta;

/// More parts than this and the engine is asked for a whole frame: every part is a draw call and
/// a scroll in small steps leaves a sliver behind each one.
pub const MAX_PARTS: usize = 32;

/// One image and where it is visible.
#[derive(Clone)]
pub struct Part {
    pub image: Rc<Image>,
    /// Where the image's top-left corner is, in view pixels (may be off the view).
    pub x: i32,
    pub y: i32,
    /// The part of the view the image shows in.
    pub clip: IRect,
}

impl Part {
    /// The part of the view this part paints: its clip within its image.
    pub fn visible(&self, view: &IRect) -> IRect {
        let image = IRect::new(
            self.x,
            self.y,
            self.image.width() as i32,
            self.image.height() as i32,
        );
        self.clip.intersect(&image).intersect(view)
    }
}

#[derive(Default)]
pub struct Parts {
    parts: Vec<Part>,
    size: (u32, u32),
}

impl Parts {
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.parts.len()
    }

    /// The size of the frame the picture was last made whole at.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    pub fn clear(&mut self) {
        self.parts.clear();
        self.size = (0, 0);
    }

    /// A whole frame replaces everything. The previous first image's pixels are returned when
    /// nothing else holds them, to be filled again.
    pub fn set_full(&mut self, image: Image) -> Vec<u8> {
        self.size = image.size();
        let (width, height) = (image.width() as i32, image.height() as i32);
        let old = std::mem::take(&mut self.parts);
        self.parts.push(Part {
            image: Rc::new(image),
            x: 0,
            y: 0,
            clip: IRect::new(0, 0, width, height),
        });
        old.into_iter()
            .filter_map(|part| Rc::try_unwrap(part.image).ok())
            .map(Image::into_pixels)
            .max_by_key(Vec::capacity)
            .unwrap_or_default()
    }

    /// Applies one delta: the scroll first, then each patch on top. A delta for another size than
    /// the picture's is refused (`false`, nothing changed): the engine and the window disagree
    /// and a whole frame has to follow.
    pub fn apply(&mut self, delta: Delta) -> bool {
        if self.parts.is_empty() || delta.size != self.size {
            return false;
        }
        if let Some(scroll) = delta.scroll {
            self.scroll(&scroll);
        }
        let view = IRect::new(0, 0, self.size.0 as i32, self.size.1 as i32);
        for patch in delta.patches {
            let rect = patch.rect.intersect(&view);
            if rect.is_empty() || rect != patch.rect {
                // The engine clips to the view: anything else is a bug on one side, not a picture.
                return false;
            }
            let Ok(image) = Image::from_rgba(rect.width() as u32, rect.height() as u32, patch.rgba)
            else {
                return false;
            };
            // What the patch hides entirely is not drawn any more.
            self.parts
                .retain(|part| !rect.contains(&part.visible(&view)));
            self.parts.push(Part {
                image: Rc::new(image),
                x: rect.x0,
                y: rect.y0,
                clip: rect,
            });
        }
        true
    }

    /// Moves what is inside `scroll.area` by its offset: each part keeps the piece of its clip
    /// outside the area where it was, and shows the piece inside it moved (what leaves the area
    /// is gone). Parts keep their order, so the picture stays what a single image would be.
    fn scroll(&mut self, scroll: &Scroll) {
        let view = IRect::new(0, 0, self.size.0 as i32, self.size.1 as i32);
        let area = scroll.area.intersect(&view);
        let mut moved = Vec::with_capacity(self.parts.len() + 2);
        let mut outside = Vec::new();
        for part in self.parts.drain(..) {
            let inside = part.clip.intersect(&area);
            outside.clear();
            part.clip.subtract(&area, &mut outside);
            for clip in outside.drain(..) {
                moved.push(Part {
                    clip,
                    ..part.clone()
                });
            }
            let landed = inside.translate(scroll.dx, scroll.dy).intersect(&area);
            if !landed.is_empty() {
                moved.push(Part {
                    image: part.image,
                    x: part.x.saturating_add(scroll.dx),
                    y: part.y.saturating_add(scroll.dy),
                    clip: landed,
                });
            }
        }
        // A part with nothing left to show is dropped.
        moved.retain(|part| !part.visible(&view).is_empty());
        self.parts = moved;
    }

    /// Whether the picture has grown so many parts that a whole frame should replace it.
    pub fn is_fragmented(&self) -> bool {
        self.parts.len() > MAX_PARTS
    }

    /// Paints the picture into `buffer`, `width * height` RGBA pixels as `u32`s: what the window
    /// draws, in software, for the tests.
    pub fn render_into(&self, buffer: &mut [u32], width: usize, height: usize) {
        let view = IRect::new(0, 0, width as i32, height as i32);
        for part in &self.parts {
            let shown = part.visible(&view);
            let source = part.image.pixels();
            let stride = part.image.width() as usize;
            for y in shown.y0..shown.y1 {
                for x in shown.x0..shown.x1 {
                    let at = ((y - part.y) as usize * stride + (x - part.x) as usize) * 4;
                    buffer[y as usize * width + x as usize] = u32::from_le_bytes([
                        source[at],
                        source[at + 1],
                        source[at + 2],
                        source[at + 3],
                    ]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mailbox::Patch;

    fn image(w: u32, h: u32, value: u8) -> Image {
        Image::from_rgba(w, h, vec![value; (w * h * 4) as usize]).unwrap()
    }

    fn delta(size: (u32, u32), scroll: Option<Scroll>, patches: Vec<(IRect, u8)>) -> Delta {
        Delta {
            size,
            scroll,
            patches: patches
                .into_iter()
                .map(|(rect, value)| Patch {
                    rect,
                    rgba: vec![value; (rect.width() * rect.height() * 4) as usize],
                })
                .collect(),
        }
    }

    fn shown(parts: &Parts, w: usize, h: usize) -> Vec<u32> {
        let mut buffer = vec![0u32; w * h];
        parts.render_into(&mut buffer, w, h);
        buffer
    }

    #[test]
    fn a_whole_frame_is_one_part_and_a_delta_before_it_is_refused() {
        let mut parts = Parts::default();
        assert!(!parts.apply(delta((4, 4), None, vec![])));
        parts.set_full(image(4, 4, 1));
        assert_eq!((parts.len(), parts.size()), (1, (4, 4)));
        assert!(!parts.apply(delta((5, 4), None, vec![])), "another size");
    }

    #[test]
    fn a_patch_covers_what_it_lands_on() {
        let mut parts = Parts::default();
        parts.set_full(image(4, 4, 1));
        assert!(parts.apply(delta((4, 4), None, vec![(IRect::new(1, 1, 2, 2), 9)])));
        let picture = shown(&parts, 4, 4);
        assert_eq!(picture[0], u32::from_le_bytes([1; 4]));
        assert_eq!(picture[5], u32::from_le_bytes([9; 4]));
        assert_eq!(picture[10], u32::from_le_bytes([9; 4]));
        assert_eq!(picture[15], u32::from_le_bytes([1; 4]));
    }

    #[test]
    fn a_patch_over_a_whole_part_removes_it() {
        let mut parts = Parts::default();
        parts.set_full(image(4, 4, 1));
        assert!(parts.apply(delta((4, 4), None, vec![(IRect::new(0, 0, 2, 2), 5)])));
        assert!(parts.apply(delta((4, 4), None, vec![(IRect::new(0, 0, 4, 4), 7)])));
        assert_eq!(parts.len(), 1);
    }

    #[test]
    fn a_scroll_moves_the_picture_and_the_patch_fills_what_it_uncovered() {
        let rows: Vec<u8> = (0..4).flat_map(|row| vec![row as u8 + 1; 4 * 4]).collect();
        let mut parts = Parts::default();
        parts.set_full(Image::from_rgba(4, 4, rows).unwrap());
        let scroll = Scroll {
            area: IRect::new(0, 0, 4, 4),
            dx: 0,
            dy: -1,
        };
        assert!(parts.apply(delta(
            (4, 4),
            Some(scroll),
            vec![(IRect::new(0, 3, 4, 1), 9)]
        )));
        let picture = shown(&parts, 4, 4);
        let row = |y: usize| picture[y * 4];
        assert_eq!(row(0), u32::from_le_bytes([2; 4]));
        assert_eq!(row(1), u32::from_le_bytes([3; 4]));
        assert_eq!(row(2), u32::from_le_bytes([4; 4]));
        assert_eq!(row(3), u32::from_le_bytes([9; 4]));
    }

    #[test]
    fn a_scroll_of_part_of_the_view_leaves_the_rest_where_it_was() {
        let mut parts = Parts::default();
        let mut pixels = Vec::new();
        for _ in 0..4 {
            for x in 0..4u8 {
                pixels.extend_from_slice(&[x + 1; 4]);
            }
        }
        parts.set_full(Image::from_rgba(4, 4, pixels).unwrap());
        // The right column (x = 3) does not scroll; the rest moves one pixel to the right.
        let scroll = Scroll {
            area: IRect::new(0, 0, 3, 4),
            dx: 1,
            dy: 0,
        };
        assert!(parts.apply(delta(
            (4, 4),
            Some(scroll),
            vec![(IRect::new(0, 0, 1, 4), 9)]
        )));
        let picture = shown(&parts, 4, 4);
        let at = |x: usize| picture[x];
        assert_eq!(at(0), u32::from_le_bytes([9; 4]));
        assert_eq!(at(1), u32::from_le_bytes([1; 4]));
        assert_eq!(at(2), u32::from_le_bytes([2; 4]));
        assert_eq!(
            at(3),
            u32::from_le_bytes([4; 4]),
            "outside the area it stayed"
        );
    }

    #[test]
    fn small_steps_pile_parts_up_until_a_whole_frame_is_wanted() {
        let mut parts = Parts::default();
        parts.set_full(image(8, 200, 1));
        let scroll = Scroll {
            area: IRect::new(0, 0, 8, 200),
            dx: 0,
            dy: -1,
        };
        for step in 0..(MAX_PARTS + 4) {
            assert!(parts.apply(delta(
                (8, 200),
                Some(scroll),
                vec![(IRect::new(0, 199, 8, 1), step as u8)]
            )));
        }
        assert!(parts.is_fragmented());
        parts.set_full(image(8, 200, 2));
        assert!(!parts.is_fragmented());
    }
}
