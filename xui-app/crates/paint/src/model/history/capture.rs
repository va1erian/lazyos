#![forbid(unsafe_code)]

//! Remembering what a stroke is about to overwrite.
//!
//! A stroke does not know its extent up front, and copying the whole layer at
//! the first dab would cost 64 MiB on a 4096 x 4096 layer. Instead the layer is
//! backed up in 64-pixel tiles the first time a draw touches them, and the
//! "before" picture of the stroke's bounding rectangle is assembled at the end.

use std::collections::HashMap;

use crate::model::bitmap::Bitmap;
use crate::model::rect::Rect;

/// The tile edge in pixels.
const TILE: i32 = 64;

/// Backs up the pixels one drawing action overwrites.
#[derive(Default)]
pub struct UndoCapture {
    bounds: Rect,
    tiles: HashMap<(i32, i32), Bitmap>,
}

fn tile_rect(key: (i32, i32), layer: Rect) -> Rect {
    Rect::new(key.0 * TILE, key.1 * TILE, TILE as u32, TILE as u32).intersect(layer)
}

impl UndoCapture {
    /// An empty capture.
    pub fn new() -> UndoCapture {
        UndoCapture::default()
    }

    /// Call before drawing into `rect` of `layer`: backs up every tile of it
    /// not already saved.
    pub fn touch(&mut self, layer: &Bitmap, rect: Rect) {
        let rect = rect.intersect(layer.rect());
        if rect.is_empty() {
            return;
        }
        self.bounds = self.bounds.union(rect);
        for ty in rect.y0.div_euclid(TILE)..=(rect.y1 - 1).div_euclid(TILE) {
            for tx in rect.x0.div_euclid(TILE)..=(rect.x1 - 1).div_euclid(TILE) {
                if let std::collections::hash_map::Entry::Vacant(slot) = self.tiles.entry((tx, ty))
                    && let Some(tile) = layer.crop(tile_rect((tx, ty), layer.rect()))
                {
                    slot.insert(tile);
                }
            }
        }
    }

    /// The bounding rectangle of everything touched so far.
    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    /// The rectangle and its pixels from before the action, or `None` when the
    /// action changed nothing. `layer` is the layer after drawing.
    pub fn finish(self, layer: &Bitmap) -> Option<(Rect, Bitmap)> {
        let bounds = self.bounds.intersect(layer.rect());
        let mut before = layer.crop(bounds)?;
        for (key, tile) in &self.tiles {
            let at = tile_rect(*key, layer.rect());
            let part = at.intersect(bounds);
            if part.is_empty() {
                continue;
            }
            let src = Rect::new(part.x0 - at.x0, part.y0 - at.y0, part.width(), part.height());
            before.blit(part.x0 - bounds.x0, part.y0 - bounds.y0, tile, src);
        }
        (layer.crop(bounds)? != before).then_some((bounds, before))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WHITE;

    #[test]
    fn finish_reassembles_the_picture_from_before() {
        let mut layer = Bitmap::white(300, 200);
        let original = layer.clone();
        let mut capture = UndoCapture::new();
        // Two distant touches spanning several tiles, drawn after each touch.
        for rect in [Rect::new(10, 10, 5, 5), Rect::new(200, 120, 70, 30)] {
            capture.touch(&layer, rect);
            for y in rect.y0..rect.y1 {
                for x in rect.x0..rect.x1 {
                    layer.put(x, y, [1, 2, 3, 255]);
                }
            }
        }
        let (rect, mut before) = capture.finish(&layer).unwrap();
        assert_eq!(rect, Rect::new(10, 10, 260, 140));
        // Swapping the capture back restores the original everywhere.
        assert!(layer.swap_region(rect.x0, rect.y0, &mut before));
        assert_eq!(layer, original);
    }

    #[test]
    fn an_unchanged_action_captures_nothing() {
        let layer = Bitmap::white(40, 40);
        let mut capture = UndoCapture::new();
        capture.touch(&layer, Rect::new(5, 5, 10, 10));
        assert!(capture.finish(&layer).is_none());
        assert!(UndoCapture::new().finish(&layer).is_none());
        let _ = WHITE;
    }

    #[test]
    fn only_touched_tiles_are_saved() {
        let layer = Bitmap::white(4096, 4096);
        let mut capture = UndoCapture::new();
        capture.touch(&layer, Rect::new(1000, 1000, 20, 20));
        assert!(capture.tiles.len() <= 4);
        // A touch outside the layer is ignored.
        capture.touch(&layer, Rect::new(-50, -50, 10, 10));
        assert!(capture.tiles.len() <= 4);
    }
}
