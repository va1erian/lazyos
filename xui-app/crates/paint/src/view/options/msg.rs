#![forbid(unsafe_code)]

//! What the options bar's controls say, and how the model takes it.

use crate::model::text::TextAlign;
use crate::model::tools::{Arrow, FillArea, FillMode, GradientKind, Sample, SampleSize};
use crate::model::{Model, Tool};

/// A change of one tool option. The brush width is [`Msg::Size`](crate::view::Msg::Size).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptMsg {
    /// The brush edge hardness in percent.
    Hardness(u8),
    /// Antialiasing, for the tool that is active (brush, shape or text).
    Antialias(bool),
    /// A line's arrow heads.
    Arrow(Arrow),
    /// Outline, fill or both for a closed shape.
    ShapeMode(FillMode),
    /// The Rounded Rectangle's corner radius in pixels.
    Radius(u32),
    /// The Paint Bucket's tolerance in percent.
    FillTolerance(u8),
    /// The Paint Bucket's contiguous or global area.
    FillArea(FillArea),
    /// The Paint Bucket compares the layer or the image.
    FillSample(Sample),
    /// How many pixels the Color Picker averages.
    PickSize(SampleSize),
    /// The Color Picker reads the layer or the image.
    PickSample(Sample),
    /// The Color Picker returns to the previous tool.
    PickSwitchBack(bool),
    /// The gradient's shape.
    GradientKind(GradientKind),
    /// The font, as an index into the family list.
    Family(usize),
    /// The font size in pixels.
    FontSize(u32),
    /// Bold.
    Bold(bool),
    /// Italic.
    Italic(bool),
    /// Underline.
    Underline(bool),
    /// Left, centre or right.
    Align(TextAlign),
}

impl OptMsg {
    /// Applies the change to `model`; `families` names the fonts the family
    /// index refers to.
    pub(in crate::view) fn apply(self, model: &mut Model, families: &[String]) {
        match self {
            OptMsg::Hardness(percent) => {
                let mut brush = model.brush();
                brush.hardness = percent.min(100);
                model.set_brush(brush);
            }
            OptMsg::Antialias(on) => antialias(model, on),
            OptMsg::Arrow(arrow) => update_shape(model, |s| s.arrow = arrow),
            OptMsg::ShapeMode(mode) => update_shape(model, |s| s.mode = mode),
            OptMsg::Radius(radius) => update_shape(model, |s| s.radius = radius as f32),
            OptMsg::FillTolerance(percent) => {
                let mut fill = model.fill();
                fill.tolerance = percent.min(100);
                model.set_fill(fill);
            }
            OptMsg::FillArea(area) => {
                let mut fill = model.fill();
                fill.area = area;
                model.set_fill(fill);
            }
            OptMsg::FillSample(sample) => {
                let mut fill = model.fill();
                fill.sample = sample;
                model.set_fill(fill);
            }
            OptMsg::PickSize(size) => update_picker(model, |p| p.size = size),
            OptMsg::PickSample(sample) => update_picker(model, |p| p.sample = sample),
            OptMsg::PickSwitchBack(back) => update_picker(model, |p| p.switch_back = back),
            OptMsg::GradientKind(kind) => {
                let mut gradient = model.gradient();
                gradient.kind = kind;
                model.set_gradient(gradient);
            }
            OptMsg::Family(index) => {
                if let Some(name) = families.get(index) {
                    update_text(model, |t| t.family = name.clone());
                }
            }
            OptMsg::FontSize(size) => update_text(model, |t| t.size = size.clamp(4, 500) as f32),
            OptMsg::Bold(on) => update_text(model, |t| t.bold = on),
            OptMsg::Italic(on) => update_text(model, |t| t.italic = on),
            OptMsg::Underline(on) => update_text(model, |t| t.underline = on),
            OptMsg::Align(align) => update_text(model, |t| t.align = align),
        }
    }
}

/// Antialiasing belongs to the group of the active tool.
fn antialias(model: &mut Model, on: bool) {
    match model.tool() {
        Tool::Text => update_text(model, |t| t.antialias = on),
        tool if tool.is_shape() => update_shape(model, |s| s.antialias = on),
        _ => {
            let mut brush = model.brush();
            brush.antialias = on;
            model.set_brush(brush);
        }
    }
}

fn update_shape(model: &mut Model, change: impl FnOnce(&mut crate::model::tools::ShapeOptions)) {
    let mut shape = model.shape();
    change(&mut shape);
    model.set_shape(shape);
}

fn update_picker(model: &mut Model, change: impl FnOnce(&mut crate::model::tools::PickerOptions)) {
    let mut picker = model.picker();
    change(&mut picker);
    model.set_picker(picker);
}

fn update_text(model: &mut Model, change: impl FnOnce(&mut crate::model::text::TextStyle)) {
    let mut style = model.text_style().clone();
    change(&mut style);
    model.set_text_style(style);
}
