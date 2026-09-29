#![forbid(unsafe_code)]

//! One filled and/or stroked path of an icon.

use xui_core::backend::PathSeg;

use crate::tone::Tone;

/// A stroke: a colour role and a width on the 32-unit grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Line {
    /// The colour role.
    pub tone: Tone,
    /// The width in grid units.
    pub width: f32,
}

/// One path of an icon, with the fill and stroke it is painted with.
///
/// The path is in grid units (see [`GRID`](crate::GRID)); it is filled first
/// and stroked over the fill, like an SVG element with both set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    /// The outline, in grid units.
    pub path: &'static [PathSeg],
    /// The fill role, if the path is filled.
    pub fill: Option<Tone>,
    /// The stroke, if the path is outlined.
    pub line: Option<Line>,
}

impl Shape {
    /// A filled path with no outline.
    pub const fn fill(path: &'static [PathSeg], fill: Tone) -> Shape {
        Shape {
            path,
            fill: Some(fill),
            line: None,
        }
    }

    /// An outline with no fill.
    pub const fn line(path: &'static [PathSeg], tone: Tone, width: f32) -> Shape {
        Shape {
            path,
            fill: None,
            line: Some(Line { tone, width }),
        }
    }

    /// A filled path with an outline.
    pub const fn both(path: &'static [PathSeg], fill: Tone, tone: Tone, width: f32) -> Shape {
        Shape {
            path,
            fill: Some(fill),
            line: Some(Line { tone, width }),
        }
    }
}
