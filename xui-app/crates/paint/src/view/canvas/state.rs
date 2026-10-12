#![forbid(unsafe_code)]

//! What the canvas painter reads: the flattened cache, the viewport and the
//! screen image built from them. Kept separate from the model so a painter
//! never borrows the model or mutates it.

use xui_core::geometry::Rect;
use xui_core::image::Image;

use super::tweak::AdjustPreview;
use crate::model::{Damage, Document, Edge, Rect as PixelRect, Tool};
use crate::render::{self, FlatCache};
use crate::viewport::Viewport;

/// The canvas's view-side state.
pub struct CanvasState {
    cache: FlatCache,
    viewport: Viewport,
    doc_size: (u32, u32),
    view: (i32, i32),
    /// The screen image and where it sits in the view.
    screen: Option<(Image, (i32, i32))>,
    screen_stale: bool,
    /// The active tool: Pan drags the view, Zoom shows a marquee.
    pub(super) tool: Tool,
    /// The pointer in picture pixels, while it is over the canvas.
    pub(super) cursor: Option<(i32, i32)>,
    /// The brush diameter, for the ring under the pointer.
    pub(super) brush: u32,
    /// Whether a canvas drag is in progress (Esc then cancels it).
    pub(super) dragging: bool,
    /// Whether a text block is being edited (keys and characters go to it).
    pub(super) editing: bool,
    /// The text caret as `(x, y, height)` in picture pixels.
    pub(super) caret: Option<(f32, f32, f32)>,
    /// The text block's box and its drag handle, in picture pixels.
    pub(super) block: Option<(PixelRect, PixelRect)>,
    /// The Zoom tool's rubber band, corner to corner in picture pixels.
    pub(super) marquee: Option<((i32, i32), (i32, i32))>,
    grid: bool,
    /// Whether Space is held (a drag then pans).
    pub(super) space: bool,
    /// The last pointer position of a pan drag, in view pixels.
    pub(super) pan: Option<(i32, i32)>,
    /// The selection's outline and the model revision it was built for.
    pub(super) edges: Vec<Edge>,
    pub(super) sel_rev: u64,
    /// How far the ants have crawled.
    pub(super) phase: u32,
    /// An adjustment shown on the screen image only (the adjustment dialogs).
    tweak: Option<AdjustPreview>,
    /// The window's dpi, which the checkerboard, the ants and the caret scale by.
    pub(super) dpi: u32,
}

impl Default for CanvasState {
    fn default() -> CanvasState {
        CanvasState {
            cache: FlatCache::new(),
            viewport: Viewport::new(),
            doc_size: (1, 1),
            view: (0, 0),
            screen: None,
            screen_stale: true,
            tool: Tool::Pencil,
            cursor: None,
            brush: 1,
            dragging: false,
            editing: false,
            caret: None,
            block: None,
            marquee: None,
            grid: true,
            space: false,
            pan: None,
            edges: Vec::new(),
            sel_rev: 0,
            phase: 0,
            tweak: None,
            dpi: 96,
        }
    }
}

impl CanvasState {
    /// The viewport.
    pub fn viewport(&self) -> &Viewport {
        &self.viewport
    }

    /// Takes in what the model changed. Returns whether the picture shown
    /// changed (so the node needs a repaint).
    pub(super) fn update(&mut self, doc: &Document, damage: Damage) -> bool {
        self.doc_size = doc.size();
        let mut changed = self.cache.get().is_none() || damage != Damage::None;
        if changed {
            self.cache.update(doc, damage);
            self.screen_stale = true;
        }
        if self.viewport.clamp(self.doc_size, self.view) {
            self.screen_stale = true;
            changed = true;
        }
        changed
    }

    /// Records the node's size; a change re-clamps the scroll and rebuilds the
    /// screen image. Returns whether anything moved.
    pub(super) fn set_view(&mut self, rect: Rect) -> bool {
        let view = (rect.width(), rect.height());
        if view == self.view {
            return false;
        }
        self.view = view;
        self.viewport.clamp(self.doc_size, view);
        self.screen_stale = true;
        true
    }

    /// Records the window's dpi; a change rebuilds the screen image (its
    /// checkerboard cells are scaled).
    pub(super) fn set_dpi(&mut self, dpi: u32) {
        if self.dpi != dpi {
            self.dpi = dpi;
            self.screen_stale = true;
        }
    }

    /// Applies a viewport change; marks the screen image stale when it differs.
    pub(super) fn change_viewport(
        &mut self,
        change: impl FnOnce(&mut Viewport, (u32, u32), (i32, i32)),
    ) -> bool {
        let before = self.viewport;
        change(&mut self.viewport, self.doc_size, self.view);
        let moved = self.viewport != before;
        self.screen_stale |= moved;
        moved
    }

    /// Starts, changes or ends the adjustment preview.
    pub(super) fn set_tweak(&mut self, tweak: Option<AdjustPreview>) {
        self.tweak = tweak;
        self.screen_stale = true;
    }

    /// Shows or hides the pixel grid (it only appears from 800%).
    pub(super) fn set_grid(&mut self, grid: bool) {
        if self.grid != grid {
            self.grid = grid;
            self.screen_stale = true;
        }
    }

    /// The picture pixels `a` to `b` (inclusive corners) fill the view.
    pub(super) fn zoom_to_rect(&mut self, a: (i32, i32), b: (i32, i32)) -> bool {
        let rect = PixelRect::from_points(a, b);
        self.change_viewport(|vp, doc, view| vp.fit_rect(rect, doc, view))
    }

    /// The picture pixel at the top-left of the view, kept inside the picture:
    /// where a paste lands.
    pub(super) fn top_left(&self) -> (i32, i32) {
        let (x, y) = self.viewport.to_doc(0, 0);
        (x.max(0), y.max(0))
    }

    /// The picture pixel under the view pixel `(x, y)`.
    pub(super) fn to_doc(&self, x: i32, y: i32) -> (i32, i32) {
        self.viewport.to_doc(x, y)
    }

    /// The window-sized image and its offset in the view (non-zero when the
    /// picture is centred), rebuilt first when stale. It is drawn 1:1.
    pub(super) fn screen(&mut self) -> Option<(&Image, (i32, i32))> {
        if self.screen_stale {
            self.screen_stale = false;
            self.screen = self.cache.get().and_then(|flat| {
                let tweak = self.tweak.as_ref();
                let apply = |x, y, pixel| tweak.map_or(pixel, |t| t.pixel(x, y, pixel));
                let image = render::render_at(
                    flat,
                    &self.viewport,
                    self.view,
                    render::Look {
                        grid: self.grid,
                        checker: crate::viewport::scaled_px(render::CHECKER as i32, self.dpi),
                    },
                    tweak.is_some().then_some(&apply),
                )?;
                let at = (image.x, image.y);
                let pixels = Image::from_rgba(image.width, image.height, image.rgba).ok()?;
                Some((pixels, at))
            });
        }
        self.screen.as_ref().map(|(image, at)| (image, *at))
    }
}
