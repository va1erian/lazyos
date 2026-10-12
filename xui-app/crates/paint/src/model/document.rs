#![forbid(unsafe_code)]

//! The document: tools, colours, the bitmap and the undo history.

use std::cell::Cell;

use super::history::UndoCapture;
use super::{
    Bitmap, DEFAULT_CAP, Damage, Document, History, Pixel, Rect, Reserved, Side, Tool, WHITE,
    check_budget,
};

mod edit;
mod layers;
mod ops;

/// What a rubber-banded shape preview is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    /// A line.
    Line,
    /// A rectangle outline.
    Rectangle,
    /// An ellipse outline.
    Ellipse,
}

/// A live shape preview, in canvas pixels; it is not part of the bitmap or the
/// history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preview {
    /// Which shape to draw.
    pub kind: PreviewKind,
    /// The drag anchor.
    pub from: (i32, i32),
    /// The current drag end.
    pub to: (i32, i32),
    /// The outline colour.
    pub color: Pixel,
    /// The outline thickness in pixels.
    pub diameter: u32,
}

/// An in-progress drag.
enum Drag {
    /// A freehand stroke; the layer already carries the pixels drawn so far.
    Freehand {
        layer: usize,
        last: (i32, i32),
        diameter: u32,
        color: Pixel,
        capture: UndoCapture,
    },
    /// A shape being rubber-banded; the layer is untouched until release.
    Shape { start: (i32, i32), side: Side },
}

/// The drawing document: layers, a tool choice, the two colours and a bounded
/// undo history.
pub struct Model {
    doc: Document,
    history: History,
    tool: Tool,
    size: u32,
    primary: Pixel,
    secondary: Pixel,
    preview: Option<Preview>,
    drag: Option<Drag>,
    revision: u64,
    /// What changed since the view last called [`Model::take_damage`].
    damage: Cell<Damage>,
    /// Bytes of the floating selection (P2's lift and paste set it).
    floating_bytes: u64,
    /// Bytes of the app-wide clipboard bitmap (P2's cut and copy set it).
    clipboard_bytes: u64,
}

impl Model {
    /// A model over a white one-layer document. Sides are clamped to
    /// `1..=MAX_SIDE`; a size over the memory budget falls back to the default
    /// canvas (use [`Model::try_new`] to learn why).
    pub fn new(width: u32, height: u32) -> Model {
        Model::try_new(width, height).unwrap_or_else(|_| {
            Model::try_new(crate::DEFAULT_WIDTH, crate::DEFAULT_HEIGHT)
                .expect("the default canvas fits the budget")
        })
    }

    /// Like [`Model::new`], refusing a size over the memory budget.
    pub fn try_new(width: u32, height: u32) -> Result<Model, String> {
        let history = History::new(DEFAULT_CAP);
        let doc = Document::new(width, height, WHITE).map_err(|e| e.to_string())?;
        Ok(Model {
            doc,
            history,
            tool: Tool::Pencil,
            size: 4,
            primary: [0, 0, 0, 255],
            secondary: WHITE,
            preview: None,
            drag: None,
            revision: 1,
            damage: Cell::new(Damage::All),
            floating_bytes: 0,
            clipboard_bytes: 0,
        })
    }

    /// The active layer's bitmap: the one tools draw on and the picker reads.
    pub fn bitmap(&self) -> &Bitmap {
        &self.doc.active_layer().pixels
    }

    /// The document: every layer.
    pub fn document(&self) -> &Document {
        &self.doc
    }

    /// The visible layers composited: what is shown and saved.
    pub fn flatten(&self) -> Bitmap {
        self.doc.flatten()
    }

    /// What changed since the last call, and resets it. The view's flattened
    /// cache updates exactly that.
    pub fn take_damage(&self) -> Damage {
        self.damage.take()
    }

    /// What the budget must leave room for besides the document.
    pub fn reserved(&self) -> Reserved {
        Reserved {
            floating: self.floating_bytes,
            clipboard: self.clipboard_bytes,
            history: self.history.cap() as u64,
        }
    }

    /// Checks that a `width` x `height` document of `layers` layers fits the
    /// memory budget with its flattened cache, the floating selection, the
    /// clipboard bitmap and the history cap. New, open, resize, canvas size and
    /// add or duplicate layer call it first.
    pub fn check_fits(&self, width: u32, height: u32, layers: usize) -> Result<(), String> {
        check_budget(width, height, layers, self.reserved()).map_err(|e| e.to_string())
    }

    /// Checks that a further bitmap of `extra` bytes (a paste, a cut, the
    /// lift of Move Selected Pixels) fits next to the current document.
    pub fn check_extra_fits(&self, extra: u64) -> Result<(), String> {
        let mut reserved = self.reserved();
        reserved.floating = reserved.floating.saturating_add(extra);
        let (w, h) = self.doc.size();
        check_budget(w, h, self.doc.layers().len(), reserved).map_err(|e| e.to_string())
    }

    /// Records the size of the floating selection's bitmap for the budget.
    pub fn set_floating_bytes(&mut self, bytes: u64) {
        self.floating_bytes = bytes;
    }

    /// Records the size of the clipboard bitmap for the budget.
    pub fn set_clipboard_bytes(&mut self, bytes: u64) {
        self.clipboard_bytes = bytes;
    }

    /// Marks `rect` (document pixels) as needing a repaint.
    fn damage_rect(&self, rect: Rect) {
        self.damage.set(self.damage.get().merge(Damage::Rect(rect)));
    }

    /// Marks everything as needing a repaint.
    fn damage_all(&self) {
        self.damage.set(Damage::All);
    }

    /// A monotonic id that changes with every bitmap mutation; the view rebuilds
    /// its cached [`xui_core::image::Image`] only when this changes.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The active tool.
    pub fn tool(&self) -> Tool {
        self.tool
    }

    /// The brush diameter.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// The primary colour.
    pub fn primary(&self) -> Pixel {
        self.primary
    }

    /// The secondary colour.
    pub fn secondary(&self) -> Pixel {
        self.secondary
    }

    /// The live shape preview, if a shape is being dragged.
    pub fn preview(&self) -> Option<Preview> {
        self.preview
    }

    /// The history, for the toolbar's enabled state.
    pub fn history(&self) -> &History {
        &self.history
    }
}
