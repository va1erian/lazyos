#![forbid(unsafe_code)]

//! The adjustment preview: a colour adjustment shown on the pixels the canvas
//! draws, with the document untouched.
//!
//! The Brightness/Contrast and Hue/Saturation dialogs set one while their
//! sliders move. The screen image is a window's worth of pixels, so a drag
//! costs the window, not the layer; OK later applies the adjustment to the
//! active layer as one history entry and Cancel just drops the preview.
//!
//! OK changes the active layer only, so the preview recomposes the visible
//! layers with the active one adjusted ([`LayerStack`]) rather than adjusting
//! the flattened picture: with layers above or below, or an opacity under 100%,
//! the two differ.

use std::rc::Rc;

use crate::model::{Adjustment, Document, Layer, Pixel, Selection, mix_by, source_over};

/// The visible layers of a document at the moment a dialog opened, with the
/// active one marked, for recomposing a pixel with that layer adjusted.
#[derive(Clone)]
pub struct LayerStack(Rc<Stack>);

struct Stack {
    /// The layers that take part in the picture, bottom first.
    layers: Vec<Layer>,
    /// Where the active layer is among them.
    active: usize,
}

impl LayerStack {
    /// The stack of `doc`, or `None` when the active layer is not shown (the
    /// picture then has nothing to preview).
    pub fn of(doc: &Document) -> Option<LayerStack> {
        let mut layers = Vec::new();
        let mut active = None;
        for (index, layer) in doc.layers().iter().enumerate() {
            if index == doc.active() && layer.is_shown() {
                active = Some(layers.len());
            }
            if layer.is_shown() {
                layers.push(layer.clone());
            }
        }
        active.map(|active| LayerStack(Rc::new(Stack { layers, active })))
    }

    /// The picture's pixel at `(x, y)` with `adjust` applied to the active
    /// layer's pixel, composed like `Document::flatten`.
    fn pixel(&self, x: i32, y: i32, adjust: &dyn Fn(Pixel) -> Pixel) -> Pixel {
        let stack = &*self.0;
        let mut shown = [0; 4];
        for (index, layer) in stack.layers.iter().enumerate() {
            let mut pixel = layer.pixels.get(x, y).unwrap_or([0; 4]);
            if index == stack.active {
                pixel = adjust(pixel);
            }
            if index == 0 {
                shown = if layer.opacity == 255 {
                    pixel
                } else {
                    source_over([0; 4], pixel, layer.opacity)
                };
            } else if pixel[3] != 0 {
                shown = source_over(shown, pixel, layer.opacity);
            }
        }
        shown
    }
}

/// An adjustment (and the selection it is clipped to) previewed on screen.
pub struct AdjustPreview {
    kernel: Box<dyn Fn(Pixel) -> Pixel>,
    selection: Option<Selection>,
    stack: Option<LayerStack>,
}

impl AdjustPreview {
    /// Previews `adjustment`, limited to `selection` when there is one, on the
    /// pixels the canvas draws.
    pub fn new(adjustment: Adjustment, selection: Option<Selection>) -> AdjustPreview {
        AdjustPreview {
            kernel: adjustment.kernel(),
            selection,
            stack: None,
        }
    }

    /// Previews on the active layer only: the picture is recomposed from
    /// `stack` with the adjustment applied to that layer.
    pub fn on_layer(mut self, stack: LayerStack) -> AdjustPreview {
        self.stack = Some(stack);
        self
    }

    /// What the pixel at picture `(x, y)` shows, given `pixel`, the picture's
    /// pixel there: the adjusted pixel through the selection's coverage, so the
    /// preview and OK agree at a soft edge.
    pub fn pixel(&self, x: i32, y: i32, pixel: Pixel) -> Pixel {
        let cover = self.selection.as_ref().map_or(255, |s| s.coverage(x, y));
        if cover == 0 {
            return pixel;
        }
        let adjust = |layer: Pixel| mix_by(cover, layer, (self.kernel)(layer));
        match &self.stack {
            Some(stack) => stack.pixel(x, y, &adjust),
            None => adjust(pixel),
        }
    }
}
