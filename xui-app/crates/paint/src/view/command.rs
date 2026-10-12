#![forbid(unsafe_code)]

//! The commands the menus and shortcuts name whose feature does not exist yet.
//!
//! The v1 menu ([`docs/paint-plan.md`](../../../../../docs/paint-plan.md), P1) is
//! complete from the start: every item is present, wired to a [`Command`] and
//! disabled while [`Command::available`] is false. A later phase implements a
//! command by flipping `available` and giving [`PaintApp`](super::PaintApp) an
//! arm for it, so the menu, the shortcut table and their tests need no change.

/// A command (or tool) a later phase implements.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    // Edit.
    /// Cut the selection (P2).
    Cut,
    /// Copy the selection (P2).
    Copy,
    /// Copy every visible layer of the selection (P2).
    CopyMerged,
    /// Paste as a floating selection (P2).
    Paste,
    /// Paste into a new layer (P2).
    PasteNewLayer,
    /// Paste into a new image (P2).
    PasteNewImage,
    /// Erase the selected pixels (P2).
    EraseSelection,
    /// Fill the selection with the primary colour (P2).
    FillSelection,
    /// Invert the selection (P2).
    InvertSelection,
    /// Select the whole image (P2).
    SelectAll,
    /// Drop the selection (P2).
    Deselect,
    // View.
    /// Zoom in.
    ZoomIn,
    /// Zoom out.
    ZoomOut,
    /// Fit the image to the window.
    ZoomToWindow,
    /// Show the image at 100%.
    ActualSize,
    /// Toggle the pixel grid (drawn from 800%).
    PixelGrid,
    // Image.
    /// Crop to the selection (P5).
    CropToSelection,
    /// Resize the image, scaling its pixels (P5).
    ResizeImage,
    /// Mirror the image left to right (P5).
    FlipHorizontal,
    /// Mirror the image top to bottom (P5).
    FlipVertical,
    /// Rotate the image a quarter turn clockwise (P5).
    Rotate90Cw,
    /// Rotate the image a quarter turn anticlockwise (P5).
    Rotate90Ccw,
    /// Rotate the image half a turn (P5).
    Rotate180,
    /// Merge every layer into one (P3).
    Flatten,
    // Layers.
    /// Add a transparent layer (P3).
    AddLayer,
    /// Delete the active layer (P3).
    DeleteLayer,
    /// Duplicate the active layer (P3).
    DuplicateLayer,
    /// Merge the active layer into the one below (P3).
    MergeLayerDown,
    /// Move the active layer up (P3).
    MoveLayerUp,
    /// Move the active layer down (P3).
    MoveLayerDown,
    /// Mirror the active layer left to right (P5).
    FlipLayerHorizontal,
    /// Mirror the active layer top to bottom (P5).
    FlipLayerVertical,
    /// Open the layer's properties (P3).
    LayerProperties,
    // Adjustments.
    /// Invert the colours of the layer (P5).
    InvertColors,
    /// Turn the layer to greys (P5).
    BlackAndWhite,
    /// Tint the layer sepia (P5).
    Sepia,
    /// Brightness and contrast, with a preview (P5).
    BrightnessContrast,
    /// Hue and saturation, with a preview (P5).
    HueSaturation,
    // Tools.
    /// The selection tools (P2).
    SelectTool,
    /// Move the selected pixels (P2).
    MoveTool,
    /// Zoom by clicking (P4).
    ZoomTool,
    /// Pan by dragging (P4).
    PanTool,
    /// The gradient tool (P4).
    GradientTool,
    /// The text tool (P4).
    TextTool,
}

impl Command {
    /// Every command, in menu order.
    pub const ALL: [Command; 44] = [
        Command::Cut,
        Command::Copy,
        Command::CopyMerged,
        Command::Paste,
        Command::PasteNewLayer,
        Command::PasteNewImage,
        Command::EraseSelection,
        Command::FillSelection,
        Command::InvertSelection,
        Command::SelectAll,
        Command::Deselect,
        Command::ZoomIn,
        Command::ZoomOut,
        Command::ZoomToWindow,
        Command::ActualSize,
        Command::PixelGrid,
        Command::CropToSelection,
        Command::ResizeImage,
        Command::FlipHorizontal,
        Command::FlipVertical,
        Command::Rotate90Cw,
        Command::Rotate90Ccw,
        Command::Rotate180,
        Command::Flatten,
        Command::AddLayer,
        Command::DeleteLayer,
        Command::DuplicateLayer,
        Command::MergeLayerDown,
        Command::MoveLayerUp,
        Command::MoveLayerDown,
        Command::FlipLayerHorizontal,
        Command::FlipLayerVertical,
        Command::LayerProperties,
        Command::InvertColors,
        Command::BlackAndWhite,
        Command::Sepia,
        Command::BrightnessContrast,
        Command::HueSaturation,
        Command::SelectTool,
        Command::MoveTool,
        Command::ZoomTool,
        Command::PanTool,
        Command::GradientTool,
        Command::TextTool,
    ];

    /// Whether the feature exists. A menu item for a command that is not
    /// available is disabled, and a shortcut for it only says so in the status
    /// bar.
    pub const fn available(self) -> bool {
        matches!(
            self,
            Command::ZoomIn
                | Command::ZoomOut
                | Command::ZoomToWindow
                | Command::ActualSize
                | Command::PixelGrid
        )
    }

    /// The name shown in the status bar.
    pub const fn label(self) -> &'static str {
        match self {
            Command::Cut => "Cut",
            Command::Copy => "Copy",
            Command::CopyMerged => "Copy Merged",
            Command::Paste => "Paste",
            Command::PasteNewLayer => "Paste into New Layer",
            Command::PasteNewImage => "Paste into New Image",
            Command::EraseSelection => "Erase Selection",
            Command::FillSelection => "Fill Selection",
            Command::InvertSelection => "Invert Selection",
            Command::SelectAll => "Select All",
            Command::Deselect => "Deselect All",
            Command::ZoomIn => "Zoom In",
            Command::ZoomOut => "Zoom Out",
            Command::ZoomToWindow => "Zoom to Window",
            Command::ActualSize => "Actual Size",
            Command::PixelGrid => "Pixel Grid",
            Command::CropToSelection => "Crop to Selection",
            Command::ResizeImage => "Resize",
            Command::FlipHorizontal => "Flip Horizontal",
            Command::FlipVertical => "Flip Vertical",
            Command::Rotate90Cw => "Rotate 90 Clockwise",
            Command::Rotate90Ccw => "Rotate 90 Counterclockwise",
            Command::Rotate180 => "Rotate 180",
            Command::Flatten => "Flatten",
            Command::AddLayer => "Add New Layer",
            Command::DeleteLayer => "Delete Layer",
            Command::DuplicateLayer => "Duplicate Layer",
            Command::MergeLayerDown => "Merge Layer Down",
            Command::MoveLayerUp => "Move Layer Up",
            Command::MoveLayerDown => "Move Layer Down",
            Command::FlipLayerHorizontal => "Flip Layer Horizontal",
            Command::FlipLayerVertical => "Flip Layer Vertical",
            Command::LayerProperties => "Layer Properties",
            Command::InvertColors => "Invert Colors",
            Command::BlackAndWhite => "Black and White",
            Command::Sepia => "Sepia",
            Command::BrightnessContrast => "Brightness / Contrast",
            Command::HueSaturation => "Hue / Saturation",
            Command::SelectTool => "Select",
            Command::MoveTool => "Move",
            Command::ZoomTool => "Zoom",
            Command::PanTool => "Pan",
            Command::GradientTool => "Gradient",
            Command::TextTool => "Text",
        }
    }
}
