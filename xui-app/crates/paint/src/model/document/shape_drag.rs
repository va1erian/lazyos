#![forbid(unsafe_code)]

//! The shape tools: Line, Rectangle, Rounded Rectangle and Ellipse.
//!
//! The shape is painted into the layer on every move (see [`Live`]), so the
//! preview is exactly what release commits; the history gets one entry.

use super::live::{ClipMask, Live, clip_of};
use super::{Drag, Model};
use crate::model::tools::{ShapeCoverage, ShapeOptions, constrain, pixel_center, shapes};
use crate::model::{Mods, Rect, Side, Tool};

/// A shape being rubber-banded.
pub(super) struct ShapeDrag {
    live: Live,
    start: (i32, i32),
    end: (i32, i32),
    side: Side,
    clip: Option<ClipMask>,
}

/// The corner `end` a drag with Shift held snaps to: 15 degrees for a line, a
/// square or circle otherwise.
fn constrained(tool: Tool, start: (i32, i32), end: (i32, i32), mods: Mods) -> (i32, i32) {
    match (mods.shift, tool) {
        (false, _) => end,
        (true, Tool::Line) => constrain::snap_angle_px(start, end),
        (true, _) => constrain::square(start, end),
    }
}

/// The coverage of a shape between two corner pixels.
fn coverage(
    tool: Tool,
    a: (i32, i32),
    b: (i32, i32),
    options: &ShapeOptions,
    area: Rect,
) -> ShapeCoverage {
    match tool {
        Tool::Line => ShapeCoverage {
            fill: None,
            outline: shapes::line(
                pixel_center(a.0, a.1),
                pixel_center(b.0, b.1),
                options,
                area,
            ),
        },
        Tool::Ellipse => shapes::ellipse(a, b, options, area),
        _ => shapes::rectangle(a, b, options, area),
    }
}

impl Model {
    /// The shape options the active tool draws with: a plain Rectangle has
    /// square corners whatever radius the rounded one is set to.
    fn shape_for_tool(&self) -> ShapeOptions {
        let mut options = self.shape;
        if self.tool == Tool::Rectangle {
            options.radius = 0.0;
        }
        options
    }

    pub(super) fn begin_shape(&mut self, x: i32, y: i32, side: Side) {
        let mut drag = ShapeDrag {
            live: Live::new(self.doc.active()),
            start: (x, y),
            end: (x, y),
            side,
            clip: ClipMask::of(self.doc.selection()),
        };
        self.paint_shape(&mut drag);
        self.drag = Some(Drag::Shape(drag));
    }

    pub(super) fn extend_shape(
        &mut self,
        mut drag: ShapeDrag,
        x: i32,
        y: i32,
        mods: Mods,
    ) -> ShapeDrag {
        drag.end = constrained(self.tool, drag.start, (x, y), mods);
        self.paint_shape(&mut drag);
        drag
    }

    /// Redraws the shape from `start` to `end`: outline in the clicked
    /// button's colour, fill in the other.
    fn paint_shape(&mut self, drag: &mut ShapeDrag) {
        let options = self.shape_for_tool();
        let (outline, fill) = match drag.side {
            Side::Primary => (self.primary, self.secondary),
            Side::Secondary => (self.secondary, self.primary),
        };
        let tool = self.tool;
        let Some(target) = self.doc.layer_mut(drag.live.layer) else {
            return;
        };
        let area = target.pixels.rect();
        let shape = coverage(tool, drag.start, drag.end, &options, area);
        let reach = [&shape.fill, &shape.outline]
            .into_iter()
            .flatten()
            .fold(Rect::EMPTY, |all, mask| all.union(mask.rect()));
        let clip = clip_of(&drag.clip);
        let changed = drag.live.repaint(&mut target.pixels, reach, |bitmap| {
            shape.paint(bitmap, outline, fill, clip)
        });
        self.damage_rect(changed);
        self.bump();
    }

    /// Release: the preview is the shape; record it.
    pub(super) fn end_shape(&mut self, drag: ShapeDrag) {
        let layer = drag.live.layer;
        let name = self.tool.label();
        self.commit_named(layer, drag.live.into_capture(), name);
    }

    /// Esc: the layer as it was.
    pub(super) fn cancel_shape(&mut self, drag: ShapeDrag) {
        if let Some(target) = self.doc.layer_mut(drag.live.layer) {
            let changed = drag.live.cancel(&mut target.pixels);
            self.damage_rect(changed);
            self.bump();
        }
    }
}
