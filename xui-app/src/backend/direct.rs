//! Direct painters: a node that shows a picture it keeps itself (a browser's page) writes its pixels
//! straight into the window's composed picture: the buffer slot it presents (client mode) or its frame.
//!
//! The canvas path costs a picture that changes every frame several passes over every pixel: xui
//! premultiplies and scans each new image, a partial repaint copies the damage into a scratch pixmap and
//! back, the window background is drawn under the node, and the result is copied into the frame. A
//! scrolling page at 2560x1440 is 3 to 4 megapixels a frame, so those passes were most of a frame's cost.
//! A node with a direct painter ([`set_painter`]) instead gets the destination's pixels inside its bounds
//! and writes them once ([`Target`]); in client mode that is the shared slot itself (`render.rs`).
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
use super::render::Dest;
use super::LazyOSBackend;
use crate::client_window::{intersection, subtract};

/// What a direct painter writes into: the window's composed picture (top-down RGBA, `stride` bytes a row).
/// It must write every pixel of `damage` (window coordinates, inside `bounds`), opaque, and nothing
/// outside it, and must not call back into the backend. A painter that cannot draw now returns `false`
/// having written nothing, and the damage is composed through the canvas instead: in client mode the
/// target is a buffer slot whose pixels there may be several frames old, never left on screen.
pub struct Target<'a> {
    pub pixels: &'a mut [u8],
    pub stride: usize,
    /// The node's rectangle in the window.
    pub bounds: Rect,
    /// What to write now.
    pub damage: Rect,
}

pub type DirectPainter = Rc<dyn Fn(&mut Target<'_>) -> bool>;

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

    /// Composes `damage` into `dest` with a direct painter where one may draw, and through the canvas
    /// around it. `None` when no direct painter applies (the caller composes it all through the canvas).
    pub(super) fn composite_direct(&self, window: WindowId, damage: Rect, dest: Dest) -> Option<bool> {
        if !has_any() {
            return None;
        }
        let (painter, bounds, direct) = self.direct_target(window, damage)?;
        let painted = match dest {
            Dest::Frame => self.paint_frame(window, &painter, bounds, direct)?,
            Dest::Slot(slot) => self.paint_slot(window, slot, &painter, bounds, direct)?,
        };
        // What the damage holds outside the node, through the canvas.
        for rest in subtract(damage, painted) {
            if !self.composite_canvas(window, rest, dest) {
                return Some(false);
            }
        }
        Some(true)
    }

    /// Runs `painter` on the window's frame; the rectangle it wrote, `None` (nothing written) while the
    /// frame is not the window's yet (a first frame or a resize is composed whole through the canvas) or
    /// when the painter declined.
    fn paint_frame(&self, window: WindowId, painter: &DirectPainter, bounds: Rect, direct: Rect) -> Option<Rect> {
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
        let painted = painter(&mut Target {
            pixels: &mut frame,
            stride,
            bounds,
            damage: direct,
        });
        if let Some(entry) = self.windows.borrow_mut().get_mut(&window.raw()) {
            entry.frame = frame;
        }
        painted.then_some(direct)
    }

    /// Runs `painter` on buffer slot `slot` of the window, free and prepared for this frame; the rectangle
    /// it wrote, `None` when the slot is not of the window's size, the painter declined, or the window
    /// closed under it.
    fn paint_slot(
        &self,
        window: WindowId,
        slot: u32,
        painter: &DirectPainter,
        bounds: Rect,
        direct: Rect,
    ) -> Option<Rect> {
        let (va, len, stride, direct) = {
            let windows = self.windows.borrow();
            let entry = windows.get(&window.raw())?;
            let (width, height) = (entry.width, entry.height);
            let (va, len) = entry.client.as_ref()?.slots.mapping(slot, width, height)?;
            let direct = intersection(direct, Rect::new(0, 0, width, height))?;
            (va, len, width as usize * 4, direct)
        };
        // The window cannot close under the painter: `close_window` keeps the buffers of the window being
        // composed open until `composing` is cleared.
        self.composing.set(Some(window.raw()));
        // SAFETY: `mapping` checked the slot holds a buffer of exactly `len` bytes mapped at `va`, which
        // stays mapped until the slot is closed, and closing it is deferred while `composing` names this
        // window. The slot is free (acquired, not yet submitted), so the compositor does not read it, and
        // nothing else in this task touches it until the painter returns: `Slots` only writes a slot from
        // `prepare` and `write`, which are not reached from a direct painter.
        let pixels = unsafe { core::slice::from_raw_parts_mut(va as *mut u8, len) };
        let painted = painter(&mut Target {
            pixels,
            stride,
            bounds,
            damage: direct,
        });
        self.composing.set(None);
        let closed = self.closing.borrow_mut().take();
        if let Some(closed) = closed {
            self.close_client_window(closed);
            return None;
        }
        painted.then_some(direct)
    }
}
