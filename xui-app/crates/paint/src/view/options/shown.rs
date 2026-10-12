#![forbid(unsafe_code)]

//! Which controls the options bar shows for a tool.

use crate::model::Tool;

/// The control groups a tool offers. A tool with none leaves the bar with just
/// its name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shown {
    /// The width of the brush, the eraser and the shapes.
    pub size: bool,
    /// The brush edge hardness.
    pub hardness: bool,
    /// The antialiasing check box.
    pub antialias: bool,
    /// A line's arrow heads.
    pub arrow: bool,
    /// Outline, fill or both.
    pub style: bool,
    /// The corner radius.
    pub radius: bool,
    /// How a new selection joins the old one.
    pub select_mode: bool,
    /// The Magic Wand's tolerance and contiguity.
    pub wand: bool,
    /// The Paint Bucket's options.
    pub bucket: bool,
    /// The Color Picker's options.
    pub picker: bool,
    /// The gradient's shape.
    pub gradient: bool,
    /// The font, size, styles and alignment.
    pub text: bool,
}

impl Shown {
    /// The groups `tool` offers.
    pub fn of(tool: Tool) -> Shown {
        let none = Shown::default();
        match tool {
            Tool::Brush | Tool::Eraser => Shown {
                size: true,
                hardness: true,
                antialias: true,
                ..none
            },
            Tool::Line => Shown {
                size: true,
                antialias: true,
                arrow: true,
                ..none
            },
            Tool::Rectangle | Tool::Ellipse => Shown {
                size: true,
                antialias: true,
                style: true,
                ..none
            },
            Tool::RoundedRectangle => Shown {
                size: true,
                antialias: true,
                style: true,
                radius: true,
                ..none
            },
            Tool::Fill => Shown {
                bucket: true,
                ..none
            },
            Tool::Picker => Shown {
                picker: true,
                ..none
            },
            Tool::Gradient => Shown {
                gradient: true,
                ..none
            },
            Tool::Text => Shown {
                text: true,
                antialias: true,
                ..none
            },
            Tool::MagicWand => Shown {
                select_mode: true,
                wand: true,
                ..none
            },
            Tool::RectSelect | Tool::EllipseSelect | Tool::Lasso => Shown {
                select_mode: true,
                ..none
            },
            Tool::Pencil
            | Tool::MovePixels
            | Tool::MoveSelection
            | Tool::Zoom
            | Tool::Pan => none,
        }
    }
}
