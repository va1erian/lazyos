#![forbid(unsafe_code)]

//! The tool vocabulary and the brush sizes.

/// The Brush sizes offered by the toolbar, in pixels.
pub const SIZES: [u32; 5] = [1, 2, 4, 8, 16];

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
    /// A rectangle outline.
    Rectangle,
    /// An ellipse outline.
    Ellipse,
    /// A flood fill with the clicked colour.
    Fill,
    /// The eyedropper: picks a colour from the canvas.
    Picker,
}

impl Tool {
    /// Every tool, in toolbar order.
    pub const ALL: [Tool; 8] = [
        Tool::Pencil,
        Tool::Brush,
        Tool::Eraser,
        Tool::Line,
        Tool::Rectangle,
        Tool::Ellipse,
        Tool::Fill,
        Tool::Picker,
    ];

    /// Whether the tool draws a rubber-banded shape (preview, then commit).
    pub const fn is_shape(self) -> bool {
        matches!(self, Tool::Line | Tool::Rectangle | Tool::Ellipse)
    }

    /// Whether the tool paints freehand while dragging.
    pub const fn is_freehand(self) -> bool {
        matches!(self, Tool::Pencil | Tool::Brush | Tool::Eraser)
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
