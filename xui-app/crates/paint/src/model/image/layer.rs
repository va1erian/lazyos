#![forbid(unsafe_code)]

//! One layer of a document.

use crate::model::bitmap::Bitmap;

/// A named, straight-alpha RGBA layer with a visibility flag and an opacity.
/// Blending is Normal only in v1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layer {
    /// The name shown in the Layers panel.
    pub name: String,
    /// The layer's pixels, the size of the document.
    pub pixels: Bitmap,
    /// Whether the layer takes part in the flattened picture.
    pub visible: bool,
    /// How opaque the whole layer is, 0 (gone) to 255 (as painted).
    pub opacity: u8,
}

impl Layer {
    /// A visible, fully opaque layer over `pixels`.
    pub fn new(name: impl Into<String>, pixels: Bitmap) -> Layer {
        Layer {
            name: name.into(),
            pixels,
            visible: true,
            opacity: 255,
        }
    }

    /// This layer's name, visibility and opacity over other `pixels`.
    pub fn with_pixels(&self, pixels: Bitmap) -> Layer {
        Layer {
            name: self.name.clone(),
            pixels,
            visible: self.visible,
            opacity: self.opacity,
        }
    }

    /// The name, visibility and opacity, without the pixels.
    pub fn props(&self) -> LayerProps {
        LayerProps {
            name: self.name.clone(),
            visible: self.visible,
            opacity: self.opacity,
        }
    }

    /// Whether the layer contributes to the flattened picture.
    pub fn is_shown(&self) -> bool {
        self.visible && self.opacity > 0
    }
}

/// The editable properties of a layer (Layer Properties).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerProps {
    /// The name.
    pub name: String,
    /// Visibility.
    pub visible: bool,
    /// Opacity.
    pub opacity: u8,
}
