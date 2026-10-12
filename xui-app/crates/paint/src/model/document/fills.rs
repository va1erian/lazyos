#![forbid(unsafe_code)]

//! The tools that fill an area: Gradient and Paint Bucket, and the Color
//! Picker that reads one.

use super::live::{ClipMask, Live, clip_of};
use super::{Drag, Model};
use crate::model::tools::{Gradient, PaintMode, StrokeBuffer, bucket_mask_in, pick};
use crate::model::{Edit, Rect, Side};

/// A gradient being dragged out.
pub(super) struct GradientDrag {
    live: Live,
    start: (i32, i32),
    end: (i32, i32),
    side: Side,
    clip: Option<ClipMask>,
}

impl Model {
    pub(super) fn begin_gradient(&mut self, x: i32, y: i32, side: Side) {
        self.drag = Some(Drag::Gradient(GradientDrag {
            live: Live::new(self.doc.active()),
            start: (x, y),
            end: (x, y),
            side,
            clip: ClipMask::of(self.doc.selection()),
        }));
    }

    pub(super) fn extend_gradient(&mut self, mut drag: GradientDrag, x: i32, y: i32) -> GradientDrag {
        if drag.end != (x, y) {
            drag.end = (x, y);
            self.paint_gradient(&mut drag);
        }
        drag
    }

    /// Redraws the gradient over the selection (the whole layer when there is
    /// none). A drag of no length paints nothing: there is no direction yet.
    fn paint_gradient(&mut self, drag: &mut GradientDrag) {
        let (from, to) = match drag.side {
            Side::Primary => (self.primary, self.secondary),
            Side::Secondary => (self.secondary, self.primary),
        };
        let center = |p: (i32, i32)| (p.0 as f32 + 0.5, p.1 as f32 + 0.5);
        let gradient = Gradient::new(self.gradient, center(drag.start), center(drag.end), from, to);
        let Some(target) = self.doc.layer_mut(drag.live.layer) else {
            return;
        };
        let area = drag
            .clip
            .as_ref()
            .map_or(target.pixels.rect(), ClipMask::rect)
            .intersect(target.pixels.rect());
        let clip = clip_of(&drag.clip);
        let changed = drag.live.repaint(&mut target.pixels, area, |bitmap| {
            gradient.paint(bitmap, area, clip)
        });
        self.damage_rect(changed);
        self.bump();
    }

    pub(super) fn end_gradient(&mut self, drag: GradientDrag) {
        let layer = drag.live.layer;
        let name = self.tool.label();
        self.commit_named(layer, drag.live.into_capture(), name);
    }

    pub(super) fn cancel_gradient(&mut self, drag: GradientDrag) {
        if let Some(target) = self.doc.layer_mut(drag.live.layer) {
            let changed = drag.live.cancel(&mut target.pixels);
            self.damage_rect(changed);
            self.bump();
        }
    }

    /// The Paint Bucket: fills the region the click selects (by tolerance,
    /// contiguous or global, on the layer or the image) with the button's
    /// colour, through the selection, as one history entry.
    pub(super) fn fill_at(&mut self, x: i32, y: i32, side: Side) {
        let color = self.color_for(side);
        let Some(mask) = bucket_mask_in(&self.doc, (x, y), &self.fill) else {
            return;
        };
        let layer = self.doc.active();
        let clip = ClipMask::of(self.doc.selection());
        let Some(target) = self.doc.layer_mut(layer) else {
            return;
        };
        let mut buffer = StrokeBuffer::new(&target.pixels);
        buffer.add_coverage(&target.pixels, &mask);
        let changed = buffer.composite(&mut target.pixels, color, PaintMode::Paint, clip_of(&clip));
        self.record_buffer(layer, buffer, changed);
    }

    /// Records a finished buffer as one history entry named for the tool,
    /// unless it changed nothing.
    pub(super) fn record_buffer(&mut self, layer: usize, buffer: StrokeBuffer, changed: Rect) {
        self.damage_rect(changed);
        let Some((rect, before)) = buffer.before() else {
            return;
        };
        let Some(target) = self.doc.layer(layer) else {
            return;
        };
        if target.pixels.crop(rect).as_ref() == Some(&before) {
            return;
        }
        self.history.record(Edit::Pixels {
            name: self.tool.label().to_string(),
            layer,
            rect,
            before,
        });
        self.bump();
    }

    /// The Color Picker: samples `(x, y)` into the clicked side's colour, then
    /// returns to the previous tool if the options ask for it.
    pub(super) fn pick_at(&mut self, x: i32, y: i32, side: Side) {
        let Some(pixel) = pick(&self.doc, x, y, &self.picker) else {
            return;
        };
        match side {
            Side::Primary => self.primary = pixel,
            Side::Secondary => self.secondary = pixel,
        }
        if self.picker.switch_back {
            self.set_tool(self.prev_tool);
        }
    }
}
