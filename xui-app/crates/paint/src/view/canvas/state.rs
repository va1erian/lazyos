#![forbid(unsafe_code)]

//! What the canvas painter reads: the flattened cache, the viewport and the
//! screen image built from them. Kept separate from the model so a painter
//! never borrows the model or mutates it.

use xui_core::geometry::Rect;
use xui_core::image::Image;

use crate::model::{Damage, Document, Preview};
use crate::render::{self, FlatCache};
use crate::viewport::Viewport;

/// The canvas's view-side state.
pub struct CanvasState {
    cache: FlatCache,
    viewport: Viewport,
    doc_size: (u32, u32),
    view: (i32, i32),
    screen: Option<Image>,
    screen_stale: bool,
    pub(super) preview: Option<Preview>,
    /// The pointer in picture pixels, while it is over the canvas.
    pub(super) cursor: Option<(i32, i32)>,
    pub(super) brush: u32,
    grid: bool,
    /// Whether Space is held (a drag then pans).
    pub(super) space: bool,
    /// The last pointer position of a pan drag, in view pixels.
    pub(super) pan: Option<(i32, i32)>,
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
            preview: None,
            cursor: None,
            brush: 1,
            grid: true,
            space: false,
            pan: None,
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

    /// Applies a viewport change; marks the screen image stale when it differs.
    pub(super) fn change_viewport(&mut self, change: impl FnOnce(&mut Viewport, (u32, u32), (i32, i32))) -> bool {
        let before = self.viewport;
        change(&mut self.viewport, self.doc_size, self.view);
        let moved = self.viewport != before;
        self.screen_stale |= moved;
        moved
    }

    /// Shows or hides the pixel grid (it only appears from 800%).
    pub(super) fn set_grid(&mut self, grid: bool) {
        if self.grid != grid {
            self.grid = grid;
            self.screen_stale = true;
        }
    }

    /// The picture pixel under the view pixel `(x, y)`.
    pub(super) fn to_doc(&self, x: i32, y: i32) -> (i32, i32) {
        self.viewport.to_doc(x, y)
    }

    /// The window-sized image, rebuilt first when stale. It is drawn 1:1.
    pub(super) fn screen(&mut self) -> Option<&Image> {
        if self.screen_stale {
            self.screen_stale = false;
            self.screen = self.cache.get().and_then(|flat| {
                let image = render::render(flat, &self.viewport, self.view, self.grid)?;
                Image::from_rgba(image.width, image.height, image.rgba).ok()
            });
        }
        self.screen.as_ref()
    }
}
