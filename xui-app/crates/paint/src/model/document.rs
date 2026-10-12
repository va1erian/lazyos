#![forbid(unsafe_code)]

//! The document: tools, colours, the bitmap and the undo history.

use std::cell::Cell;
use std::rc::Rc;

use super::text::{TextRasterizer, TextStyle};
use super::tools::{BrushOptions, FillOptions, GradientOptions, PickerOptions, ShapeOptions};
use super::{
    Bitmap, CombineMode, DEFAULT_CAP, Damage, Document, History, Pixel, Rect, Reserved, Selection,
    Side, Tool, WHITE, check_budget,
};

mod adjust;
mod clip;
mod edit;
mod fills;
mod float;
mod image_ops;
mod io;
mod layers;
mod live;
mod ops;
mod select;
mod shape_drag;
mod strokes;
mod text;
mod tool_options;

pub use clip::Clipboard;
pub use text::TextKey;

/// An in-progress drag.
enum Drag {
    /// A Pencil, Paintbrush or Eraser stroke; the layer already carries the
    /// pixels drawn so far.
    Stroke(strokes::Stroke),
    /// A shape being rubber-banded; the layer shows it, live.
    Shape(shape_drag::ShapeDrag),
    /// A gradient being dragged out; the layer shows it, live.
    Gradient(fills::GradientDrag),
    /// The text block being dragged by its handle.
    MoveText { start: (i32, i32), from: (i32, i32) },
    /// A rectangle or ellipse selection being dragged out; the document's
    /// selection already shows the result.
    Select {
        base: Option<Selection>,
        mode: CombineMode,
        start: (i32, i32),
        moved: bool,
    },
    /// A lasso being drawn.
    Lasso {
        base: Option<Selection>,
        mode: CombineMode,
        points: Vec<(i32, i32)>,
    },
    /// The selection's outline being dragged (Move Selection).
    MoveOutline {
        base: Option<Selection>,
        start: (i32, i32),
    },
    /// The floating selection being dragged.
    MoveFloat { start: (i32, i32), from: (i32, i32) },
}

/// The drawing document: layers, a tool choice, the two colours and a bounded
/// undo history.
pub struct Model {
    doc: Document,
    history: History,
    tool: Tool,
    /// The tool before this one, where the Color Picker can switch back to.
    prev_tool: Tool,
    primary: Pixel,
    secondary: Pixel,
    /// The Paintbrush and Eraser; its width is the shapes' too.
    brush: BrushOptions,
    shape: ShapeOptions,
    fill: FillOptions,
    gradient: GradientOptions,
    picker: PickerOptions,
    text_style: TextStyle,
    /// Where the last Pencil stroke ended, for Shift-click lines.
    pencil_last: Option<(i32, i32)>,
    drag: Option<Drag>,
    /// The Text tool's rasterizer, if the host supplied fonts.
    rasterizer: Option<Rc<dyn TextRasterizer>>,
    /// The text block being edited.
    text: Option<text::TextSession>,
    text_geom: Option<text::TextGeom>,
    text_handle_px: u32,
    revision: u64,
    /// What changed since the view last called [`Model::take_damage`].
    damage: Cell<Damage>,
    /// Bytes of the floating selection (P2's lift and paste set it).
    floating_bytes: u64,
    /// Bytes of the app-wide clipboard bitmap (P2's cut and copy set it).
    clipboard_bytes: u64,
    /// How a new selection joins the old one when no modifier says otherwise.
    sel_mode: CombineMode,
    /// The Magic Wand's tolerance in percent (0-100).
    wand_tolerance: u8,
    /// Whether the Magic Wand selects only the region around the click.
    wand_contiguous: bool,
    /// Changes whenever the selection may have; the view redraws its ants then.
    sel_rev: u64,
    /// Lifted or pasted pixels that are still being placed.
    float: Option<float::Float>,
    /// The app-wide clipboard slot.
    clipboard: Option<Clipboard>,
    /// Why the last tool action did nothing, for the status bar.
    notice: Option<String>,
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
            prev_tool: Tool::Pencil,
            primary: [0, 0, 0, 255],
            secondary: WHITE,
            brush: BrushOptions {
                width: 4,
                ..BrushOptions::default()
            },
            shape: ShapeOptions {
                width: 4.0,
                radius: 12.0,
                ..ShapeOptions::default()
            },
            fill: FillOptions::default(),
            gradient: GradientOptions::default(),
            picker: PickerOptions::default(),
            text_style: TextStyle::default(),
            pencil_last: None,
            drag: None,
            rasterizer: None,
            text: None,
            text_geom: None,
            text_handle_px: 8,
            revision: 1,
            damage: Cell::new(Damage::All),
            floating_bytes: 0,
            clipboard_bytes: 0,
            sel_mode: CombineMode::Replace,
            wand_tolerance: 30,
            wand_contiguous: true,
            sel_rev: 1,
            float: None,
            clipboard: None,
            notice: None,
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

    /// The width of the brush, the eraser and the shapes.
    pub fn size(&self) -> u32 {
        self.brush.width
    }

    /// The primary colour.
    pub fn primary(&self) -> Pixel {
        self.primary
    }

    /// The secondary colour.
    pub fn secondary(&self) -> Pixel {
        self.secondary
    }

    /// The history, for the toolbar's enabled state.
    pub fn history(&self) -> &History {
        &self.history
    }
}

#[cfg(test)]
mod image_tests;

#[cfg(test)]
mod alpha_tests;
