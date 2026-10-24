//! A cursor theme: the shapes as data (docs/cursor-editor-plan.md V1).
//!
//! Coordinates are design units, 512 per side of the 32x32 design square (one
//! grid cell is 16 units), the unit the rasteriser takes. A slot that is
//! `None` falls back to the built-in shape, so a theme may restyle only the
//! arrow. A theme comes from a file a user wrote, so it is bounded: a bad
//! one is refused whole by [`crate::format::parse`].

use alloc::string::String;
use alloc::vec::Vec;

use crate::raster::Layer;
use crate::{shapes, Shape, SHAPE_COUNT};

/// Design units per side of the design square.
pub const DESIGN: i32 = 512;
/// Most layers one shape may have.
pub const MAX_LAYERS: usize = 64;
/// Most vertices one shape may have, over all its layers. Rendering cost
/// grows with edges times pixels and runs in the compositor, so this is kept
/// small; a theme also has to fit confd's 4 KiB value, about 600 vertices.
pub const MAX_VERTICES: usize = 512;
/// Longest theme name.
pub const MAX_NAME: usize = 32;

/// One polygon set drawn in one colour with an outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerDef {
    pub polys: Vec<Vec<(i32, i32)>>,
    /// `0xRRGGBB`.
    pub fill: u32,
    /// `0xRRGGBB`.
    pub outline: u32,
    /// A thinner outline.
    pub thin: bool,
}

/// One shape: its layers, bottom first, and the pointer position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapeDef {
    pub hot: (i32, i32),
    pub layers: Vec<LayerDef>,
}

/// A named set of shape overrides.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Theme {
    pub name: String,
    pub shapes: [Option<ShapeDef>; SHAPE_COUNT],
}

/// Whether `name` may name a theme: `[a-z0-9-]{1,32}`.
pub fn valid_name(name: &str) -> bool {
    (1..=MAX_NAME).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl ShapeDef {
    /// Whether every coordinate is inside the design square and the shape is
    /// within the limits (the same rules the parser enforces).
    pub fn is_valid(&self) -> bool {
        let inside = |(x, y): (i32, i32)| (0..=DESIGN).contains(&x) && (0..=DESIGN).contains(&y);
        let vertices: usize = self
            .layers
            .iter()
            .flat_map(|layer| &layer.polys)
            .map(Vec::len)
            .sum();
        // A layer may be empty (a colour waiting for its first polygon) but
        // the shape must draw something.
        vertices > 0
            && self.layers.len() <= MAX_LAYERS
            && vertices <= MAX_VERTICES
            && inside(self.hot)
            && self.layers.iter().all(|layer| {
                layer
                    .polys
                    .iter()
                    .all(|poly| poly.len() >= 3 && poly.iter().all(|&p| inside(p)))
            })
    }

    pub(crate) fn to_layers(&self) -> Vec<Layer> {
        self.layers
            .iter()
            .map(|layer| Layer {
                polys: layer.polys.clone(),
                fill: layer.fill,
                outline: layer.outline,
                thin: layer.thin,
            })
            .collect()
    }
}

impl Theme {
    /// Whether the theme draws `shape` itself (a custom drawing is static,
    /// so a custom Busy does not animate).
    pub fn draws(&self, shape: Shape) -> bool {
        self.shapes[shape as usize]
            .as_ref()
            .is_some_and(ShapeDef::is_valid)
    }

    /// The built-in shapes as a theme: the editor's starting point. Busy is
    /// left out (its spinner is generated, not drawn).
    pub fn builtin(name: &str) -> Theme {
        let mut theme = Theme {
            name: name.into(),
            ..Theme::default()
        };
        for shape in Shape::ALL {
            if shape == Shape::Busy {
                continue;
            }
            let layers = shapes::layers(shape, 0)
                .into_iter()
                .map(|layer| LayerDef {
                    polys: layer.polys,
                    fill: layer.fill,
                    outline: layer.outline,
                    thin: layer.thin,
                })
                .collect();
            theme.shapes[shape as usize] = Some(ShapeDef {
                hot: shapes::hotspot(shape),
                layers,
            });
        }
        theme
    }
}
