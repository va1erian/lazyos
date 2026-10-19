//! Direct painters: a node that shows a picture it keeps itself (a browser's page) writes its pixels
//! straight into the window's composed frame.
//!
//! The canvas path costs a picture that changes every frame several passes over every pixel: xui
//! premultiplies and scans each new image, a partial repaint copies the damage into a scratch pixmap and
//! back, the window background is drawn under the node, and the result is copied into the frame. A
//! scrolling page at 2560x1440 is 3 to 4 megapixels a frame, so those passes were most of a frame's cost.
//! A node with a direct painter ([`set_painter`]) instead gets the frame's pixels inside its bounds and
//! writes them once ([`Target`]).
//!
//! It is only used when nothing would be drawn over the node in the damaged rectangle (no later node
//! overlaps it): then what the painter writes is what the canvas path would have composed. Otherwise the
//! frame is composed through the canvas as always, with the node's ordinary painter, which must draw the
//! same picture. The surface under a directly painted rectangle is left as it was; the next canvas
//! composite of it repaints it whole (every painter that reaches the damage runs), so the two stay in step.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use xui_core::backend::{WidgetId, WindowId};
use xui_core::Rect;

use super::geometry::{absolute_bounds, ancestor_clip, effectively_visible};
use super::LazyOSBackend;

/// What a direct painter writes into: the window's composed frame (top-down RGBA, `stride` bytes a row).
/// It must write every pixel of `damage` (window coordinates, inside `bounds`), opaque, and nothing
/// outside it.
pub struct Target<'a> {
    pub pixels: &'a mut [u8],
    pub stride: usize,
    /// The node's rectangle in the window.
    pub bounds: Rect,
    /// What to write now.
    pub damage: Rect,
}

pub type DirectPainter = Rc<dyn Fn(&mut Target<'_>)>;

thread_local! {
    /// By node: a backend lives on one thread, and so do its nodes and their painters.
    static PAINTERS: RefCell<HashMap<WidgetId, DirectPainter>> = RefCell::new(HashMap::new());
}

/// Gives `widget` a direct painter, or takes it away (`None`). The node keeps its ordinary painter for the
/// frames the direct one cannot draw.
pub fn set_painter(widget: WidgetId, painter: Option<DirectPainter>) {
    PAINTERS.with(|painters| {
        let mut painters = painters.borrow_mut();
        match painter {
            Some(painter) => painters.insert(widget, painter),
            None => painters.remove(&widget),
        };
    });
}

fn painter_of(widget: WidgetId) -> Option<DirectPainter> {
    PAINTERS.with(|painters| painters.borrow().get(&widget).cloned())
}

/// Whether `widget` has a direct painter.
pub(super) fn is_direct(widget: WidgetId) -> bool {
    PAINTERS.with(|painters| painters.borrow().contains_key(&widget))
}

pub(super) fn has_any() -> bool {
    PAINTERS.with(|painters| !painters.borrow().is_empty())
}

/// `rect` minus `hole`: at most four rectangles (above, below, left, right of the hole).
pub(super) fn subtract(rect: Rect, hole: Rect) -> Vec<Rect> {
    let cut = Rect::new(
        rect.left.max(hole.left),
        rect.top.max(hole.top),
        rect.right.min(hole.right),
        rect.bottom.min(hole.bottom),
    );
    if cut.left >= cut.right || cut.top >= cut.bottom {
        return vec![rect];
    }
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

fn intersection(a: Rect, b: Rect) -> Option<Rect> {
    let r = Rect::new(a.left.max(b.left), a.top.max(b.top), a.right.min(b.right), a.bottom.min(b.bottom));
    (r.left < r.right && r.top < r.bottom).then_some(r)
}

impl LazyOSBackend {
    /// The node of `window` with a direct painter that `damage` reaches, its rectangle in the window and the
    /// part of `damage` it may write directly; `None` when there is none, or when something is drawn over it
    /// there (a node created after it, visible, with a painter, near that part).
    fn direct_target(&self, window: WindowId, damage: Rect) -> Option<(DirectPainter, Rect, Rect)> {
        let nodes = self.nodes.borrow();
        let (index, id, painter) = nodes.iter().enumerate().find_map(|(index, (id, node))| {
            (node.window == window).then(|| painter_of(*id)).flatten().map(|painter| (index, *id, painter))
        })?;
        if !effectively_visible(&nodes, id) {
            return None;
        }
        // The node's own rectangle places its picture; an ancestor's clip only limits what is written.
        let bounds = absolute_bounds(&nodes, id)?;
        let mut visible = bounds;
        if let Some(clip) = ancestor_clip(&nodes, id) {
            visible = intersection(visible, clip)?;
        }
        let direct = intersection(damage, visible)?;
        // Only a node that overlaps the rectangle covers it. A neighbour's painter may spill a pixel or two
        // past its bounds (a focus ring), which the direct paint then hides at the edge: the price of not
        // sending every frame through the canvas because the status bar touches the page.
        let covered = nodes[index + 1..].iter().any(|(other, node)| {
            node.window == window
                && node.painter.is_some()
                && effectively_visible(&nodes, *other)
                && absolute_bounds(&nodes, *other).is_some_and(|rect| intersection(rect, direct).is_some())
        });
        (!covered).then_some((painter, bounds, direct))
    }

    /// Composes `damage` with a direct painter where one may draw, and through the canvas around it. `None`
    /// when no direct painter applies (the caller composes it all through the canvas).
    pub(super) fn composite_direct(&self, window: WindowId, damage: Rect) -> Option<bool> {
        if !has_any() {
            return None;
        }
        let (painter, bounds, direct) = self.direct_target(window, damage)?;
        // The frame must be the window's already (a first frame or a resize is composed whole), and
        // nothing is written outside it.
        let (mut frame, stride, direct) = {
            let mut windows = self.windows.borrow_mut();
            let entry = windows.get_mut(&window.raw())?;
            let (width, height) = (entry.width.max(0), entry.height.max(0));
            if entry.frame.len() != width as usize * height as usize * 4 {
                return None;
            }
            let direct = intersection(direct, Rect::new(0, 0, width, height))?;
            (std::mem::take(&mut entry.frame), width as usize * 4, direct)
        };
        painter(&mut Target {
            pixels: &mut frame,
            stride,
            bounds,
            damage: direct,
        });
        if let Some(entry) = self.windows.borrow_mut().get_mut(&window.raw()) {
            entry.frame = frame;
        }
        // What the damage holds outside the node, through the canvas.
        for rest in subtract(damage, direct) {
            if !self.composite_canvas(window, rest) {
                return Some(false);
            }
        }
        Some(true)
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
    }
}
