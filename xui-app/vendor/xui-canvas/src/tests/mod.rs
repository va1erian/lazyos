#![forbid(unsafe_code)]

use std::rc::Rc;

use xui_core::app::{App, Ui, run_app};
use xui_core::backend::{Backend, NodeKind, NodeSpec, ParentRef, PlatformSpec};
use xui_core::geometry::{Point, Rect};
use xui_core::image::Image;
use xui_core::widget::{Button, CheckBox, Label, ProgressBar, Slider};
use xui_core::{
    Canvas, Color, Corner, Dash, Dip, GradientStop, LinearGradient, RadialGradient, Rgba, Stroke,
    TextStyle,
};

use crate::{OffscreenBackend, RgbaImage, Surface, measure_text, to_skia};

mod paint;
mod text;
mod widgets;

/// Writes `image` to `path` as a PNG under `target/ui`.
pub(crate) fn save(path: &str, image: &RgbaImage) {
    let path = std::path::Path::new("target/ui").join(path);
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let Ok(file) = std::fs::File::create(&path) else {
        return;
    };
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    if let Ok(mut writer) = encoder.write_header() {
        let _ = writer.write_image_data(&image.pixels);
    }
}

pub(crate) fn dark_pixels(image: &RgbaImage) -> usize {
    image
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[0] < 128)
        .count()
}
