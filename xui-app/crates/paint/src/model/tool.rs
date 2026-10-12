#![forbid(unsafe_code)]

//! The tool vocabulary and the brush sizes.

/// The widths `[` and `]` step through, in pixels: fine steps for thin lines,
/// coarse ones up to the widest brush the stroke engine accepts.
pub const SIZES: [u32; 25] = [
    1, 2, 3, 4, 5, 6, 8, 10, 12, 16, 20, 24, 32, 40, 48, 64, 80, 100, 128, 160, 200, 256, 320, 400,
    500,
];

/// Which paint colour a button points at: the primary (usually left) or the
/// secondary (usually right).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// The primary colour.
    Primary,
    /// The secondary colour.
    Secondary,
}

impl Side {
    /// The other side.
    pub const fn flipped(self) -> Side {
        match self {
            Side::Primary => Side::Secondary,
            Side::Secondary => Side::Primary,
        }
    }
}

/// The modifier keys held during a pointer event. Selection tools read them
/// (Ctrl adds, Alt intersects, Shift keeps a square or circle).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    /// Ctrl (or the Windows key).
    pub ctrl: bool,
    /// Shift.
    pub shift: bool,
    /// Alt.
    pub alt: bool,
}

impl Mods {
    /// No modifier held.
    pub const NONE: Mods = Mods {
        ctrl: false,
        shift: false,
        alt: false,
    };
}

/// One drawing tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// A freehand one-pixel pen.
    Pencil,
    /// A freehand round brush of the selected size.
    Brush,
    /// A brush that paints the background colour.
    Eraser,
    /// A straight line.
    Line,
    /// A rectangle.
    Rectangle,
    /// An ellipse.
    Ellipse,
    /// A flood fill with the clicked colour.
    Fill,
    /// The eyedropper: picks a colour from the canvas.
    Picker,
    /// Selects a rectangle.
    RectSelect,
    /// Selects an ellipse.
    EllipseSelect,
    /// Selects the polygon the pointer draws.
    Lasso,
    /// Selects the pixels of a similar colour.
    MagicWand,
    /// Lifts the selected pixels and moves them.
    MovePixels,
    /// Moves the selection's outline only.
    MoveSelection,
    /// A rectangle with rounded corners.
    RoundedRectangle,
    /// A blend from the primary to the secondary colour.
    Gradient,
    /// Typed text, editable until it is committed.
    Text,
    /// Zooms the view: a click in, a right click out, a drag to a rectangle.
    Zoom,
    /// Pans the view by dragging.
    Pan,
}

impl Tool {
    /// Every tool, in toolbar order.
    pub const ALL: [Tool; 19] = [
        Tool::Pencil,
        Tool::Brush,
        Tool::Eraser,
        Tool::Line,
        Tool::Rectangle,
        Tool::Ellipse,
        Tool::Fill,
        Tool::Picker,
        Tool::RectSelect,
        Tool::EllipseSelect,
        Tool::Lasso,
        Tool::MagicWand,
        Tool::MovePixels,
        Tool::MoveSelection,
        Tool::RoundedRectangle,
        Tool::Gradient,
        Tool::Text,
        Tool::Zoom,
        Tool::Pan,
    ];

    /// Whether the tool draws a rubber-banded shape (preview, then commit).
    pub const fn is_shape(self) -> bool {
        matches!(
            self,
            Tool::Line | Tool::Rectangle | Tool::RoundedRectangle | Tool::Ellipse
        )
    }

    /// Whether the tool paints freehand while dragging.
    pub const fn is_freehand(self) -> bool {
        matches!(self, Tool::Pencil | Tool::Brush | Tool::Eraser)
    }

    /// Whether the tool makes a selection.
    pub const fn is_selector(self) -> bool {
        matches!(
            self,
            Tool::RectSelect | Tool::EllipseSelect | Tool::Lasso | Tool::MagicWand
        )
    }

    /// Whether the tool changes the view and never the picture.
    pub const fn is_view(self) -> bool {
        matches!(self, Tool::Zoom | Tool::Pan)
    }

    /// Whether the tool moves the selection or its pixels.
    pub const fn is_move(self) -> bool {
        matches!(self, Tool::MovePixels | Tool::MoveSelection)
    }

    /// A short human label for the status bar.
    pub const fn label(self) -> &'static str {
        match self {
            Tool::Pencil => "Pencil",
            Tool::Brush => "Brush",
            Tool::Eraser => "Eraser",
            Tool::Line => "Line",
            Tool::Rectangle => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Fill => "Fill",
            Tool::Picker => "Picker",
            Tool::RectSelect => "Rectangle Select",
            Tool::EllipseSelect => "Ellipse Select",
            Tool::Lasso => "Lasso Select",
            Tool::MagicWand => "Magic Wand",
            Tool::MovePixels => "Move Selected Pixels",
            Tool::MoveSelection => "Move Selection",
            Tool::RoundedRectangle => "Rounded Rectangle",
            Tool::Gradient => "Gradient",
            Tool::Text => "Text",
            Tool::Zoom => "Zoom",
            Tool::Pan => "Pan",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_is_either_freehand_shape_or_instant() {
        for tool in Tool::ALL {
            if tool.is_shape() {
                assert!(!tool.is_freehand());
            }
        }
        assert!(Tool::Line.is_shape());
        assert!(Tool::Pencil.is_freehand());
        assert!(!Tool::Fill.is_shape());
        assert!(!Tool::Picker.is_freehand());
        let _ = Tool::Eraser.label();
    }

    #[test]
    fn sides_flip() {
        assert_eq!(Side::Primary.flipped(), Side::Secondary);
        assert_eq!(Side::Secondary.flipped(), Side::Primary);
    }
}
