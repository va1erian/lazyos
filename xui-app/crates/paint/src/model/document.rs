#![forbid(unsafe_code)]

//! The document: tools, colours, the bitmap and the undo history.

use super::{Bitmap, DEFAULT_CAP, History, Pixel, Side, Tool, WHITE};

mod edit;

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
    /// A freehand stroke; the bitmap already carries the pixels drawn so far.
    Freehand {
        last: (i32, i32),
        diameter: u32,
        color: Pixel,
        before: Bitmap,
    },
    /// A shape being rubber-banded; the bitmap is untouched until release.
    Shape { start: (i32, i32), side: Side },
}

/// The drawing document: a bitmap, a tool choice, the two colours and a bounded
/// undo history.
pub struct Model {
    bitmap: Bitmap,
    history: History,
    tool: Tool,
    size: u32,
    primary: Pixel,
    secondary: Pixel,
    preview: Option<Preview>,
    drag: Option<Drag>,
    revision: u64,
}

impl Model {
    /// A model over a white bitmap of the default size.
    pub fn new(width: u32, height: u32) -> Model {
        Model {
            bitmap: Bitmap::white(width, height),
            history: History::new(DEFAULT_CAP),
            tool: Tool::Pencil,
            size: 4,
            primary: [0, 0, 0, 255],
            secondary: WHITE,
            preview: None,
            drag: None,
            revision: 1,
        }
    }

    /// The live bitmap.
    pub fn bitmap(&self) -> &Bitmap {
        &self.bitmap
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
