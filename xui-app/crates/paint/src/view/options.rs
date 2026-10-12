#![forbid(unsafe_code)]

//! The tool options bar: the active tool's name and the controls that tool
//! offers, in one row under the menu bar.
//!
//! Every control exists once; [`Options::sync`] shows the ones the tool uses
//! ([`Shown`]) and sets each from the model, so the bar is always a view of
//! the option structs ([`BrushOptions`](crate::model::tools::BrushOptions),
//! [`ShapeOptions`](crate::model::tools::ShapeOptions), ...) and a control's
//! message ([`OptMsg`]) is the only way to change one. A new option is one
//! widget in [`row`], one handle here and one line in `sync`.

use xui_core::app::Ui;
use xui_core::arrange::{
    Handle, Layout, LayoutExt, checkbox, combo_box, label, number_field, row as arrange_row,
};
use xui_core::backend::WidgetId;
use xui_core::layout::Align;
use xui_core::widget::{CheckBox, ComboBox, HasText, Label, NumberField};

use super::Msg;
use crate::model::text::TextAlign;
use crate::model::tools::{
    Arrow, FillArea, FillMode, GradientKind, MAX_BRUSH_WIDTH, Sample, SampleSize,
};
use crate::model::{CombineMode, Model, SIZES, Tool};

mod msg;
mod shown;

pub use msg::OptMsg;
pub use shown::Shown;

/// Whether `tool` paints with the width control.
pub fn uses_size(tool: Tool) -> bool {
    Shown::of(tool).size
}

/// The size after `steps` notches of `[` (negative) or `]` (positive) from
/// `current`: the neighbouring entry of [`SIZES`], clamped at both ends. A size
/// that is not an entry (the model clamps any value) steps to the nearest one
/// in that direction.
pub fn step_size(current: u32, steps: i32) -> u32 {
    let sizes = SIZES;
    let index = match steps.signum() {
        1 => sizes
            .iter()
            .position(|size| *size > current)
            .unwrap_or(sizes.len() - 1),
        -1 => sizes.iter().rposition(|size| *size < current).unwrap_or(0),
        _ => return current,
    };
    sizes[index]
}

const ARROWS: [(Arrow, &str); 4] = [
    (Arrow::None, "None"),
    (Arrow::Start, "Start"),
    (Arrow::End, "End"),
    (Arrow::Both, "Both"),
];
const SHAPE_MODES: [(FillMode, &str); 3] = [
    (FillMode::Outline, "Outline"),
    (FillMode::Fill, "Filled"),
    (FillMode::Both, "Both"),
];
const AREAS: [(FillArea, &str); 2] = [
    (FillArea::Contiguous, "Contiguous"),
    (FillArea::Global, "Global"),
];
const SAMPLES: [(Sample, &str); 2] = [(Sample::Layer, "Layer"), (Sample::Image, "Image")];
const PICK_SIZES: [(SampleSize, &str); 3] = [
    (SampleSize::One, "1 pixel"),
    (SampleSize::Three, "3 x 3"),
    (SampleSize::Five, "5 x 5"),
];
const GRADIENTS: [(GradientKind, &str); 4] = [
    (GradientKind::Linear, "Linear"),
    (GradientKind::LinearReflected, "Reflected"),
    (GradientKind::Radial, "Radial"),
    (GradientKind::Diamond, "Diamond"),
];
const ALIGNS: [(TextAlign, &str); 3] = [
    (TextAlign::Left, "Left"),
    (TextAlign::Center, "Centre"),
    (TextAlign::Right, "Right"),
];

/// The names of a table, for a combo box.
fn names<T>(table: &[(T, &'static str)]) -> Vec<&'static str> {
    table.iter().map(|(_, name)| *name).collect()
}

/// The value at a combo index, clamped to the table.
fn pick<T: Copy>(table: &[(T, &str)], index: usize) -> T {
    table[index.min(table.len() - 1)].0
}

/// A combo box over a table, selecting `value`.
fn select<T: Copy + PartialEq>(combo: &ComboBox<Msg>, table: &[(T, &str)], value: T) {
    if let Some(index) = table.iter().position(|(item, _)| *item == value) {
        if combo.selected() != index {
            combo.select(index);
        }
    }
}

/// A check box showing `on`.
fn check(boxed: &CheckBox<Msg>, on: bool) {
    if boxed.is_checked() != on {
        boxed.set_checked(on);
    }
}

/// A number field showing `value`.
fn number(field: &NumberField<Msg>, value: f64) {
    if (field.value() - value).abs() > 0.5 {
        field.set_value(value);
    }
}

/// The bar's widgets, kept to update them from the model.
pub(super) struct Options {
    tool: Handle<Label<Msg>>,
    size_cap: Handle<Label<Msg>>,
    size: Handle<NumberField<Msg>>,
    hard_cap: Handle<Label<Msg>>,
    hard: Handle<NumberField<Msg>>,
    aa: Handle<CheckBox<Msg>>,
    arrow_cap: Handle<Label<Msg>>,
    arrow: Handle<ComboBox<Msg>>,
    style_cap: Handle<Label<Msg>>,
    style: Handle<ComboBox<Msg>>,
    radius_cap: Handle<Label<Msg>>,
    radius: Handle<NumberField<Msg>>,
    mode_caption: Handle<Label<Msg>>,
    mode: Handle<ComboBox<Msg>>,
    tolerance_caption: Handle<Label<Msg>>,
    tolerance: Handle<NumberField<Msg>>,
    contiguous: Handle<CheckBox<Msg>>,
    fill_tol_cap: Handle<Label<Msg>>,
    fill_tol: Handle<NumberField<Msg>>,
    fill_area: Handle<ComboBox<Msg>>,
    fill_sample: Handle<ComboBox<Msg>>,
    pick_size: Handle<ComboBox<Msg>>,
    pick_sample: Handle<ComboBox<Msg>>,
    pick_back: Handle<CheckBox<Msg>>,
    gradient_cap: Handle<Label<Msg>>,
    gradient: Handle<ComboBox<Msg>>,
    family: Handle<ComboBox<Msg>>,
    font_size: Handle<NumberField<Msg>>,
    bold: Handle<CheckBox<Msg>>,
    italic: Handle<CheckBox<Msg>>,
    underline: Handle<CheckBox<Msg>>,
    align: Handle<ComboBox<Msg>>,
    /// The font names the family combo lists, in its order.
    families: Vec<String>,
}

impl Options {
    /// The bar for a text engine offering `families` (none for no fonts).
    pub(super) fn new(families: Vec<String>) -> Options {
        Options {
            tool: Handle::new(),
            size_cap: Handle::new(),
            size: Handle::new(),
            hard_cap: Handle::new(),
            hard: Handle::new(),
            aa: Handle::new(),
            arrow_cap: Handle::new(),
            arrow: Handle::new(),
            style_cap: Handle::new(),
            style: Handle::new(),
            radius_cap: Handle::new(),
            radius: Handle::new(),
            mode_caption: Handle::new(),
            mode: Handle::new(),
            tolerance_caption: Handle::new(),
            tolerance: Handle::new(),
            contiguous: Handle::new(),
            fill_tol_cap: Handle::new(),
            fill_tol: Handle::new(),
            fill_area: Handle::new(),
            fill_sample: Handle::new(),
            pick_size: Handle::new(),
            pick_sample: Handle::new(),
            pick_back: Handle::new(),
            gradient_cap: Handle::new(),
            gradient: Handle::new(),
            family: Handle::new(),
            font_size: Handle::new(),
            bold: Handle::new(),
            italic: Handle::new(),
            underline: Handle::new(),
            align: Handle::new(),
            families,
        }
    }

    /// The font names the family index of [`OptMsg::Family`] refers to.
    pub(super) fn families(&self) -> &[String] {
        &self.families
    }

    /// Shows `model`'s tool name, the controls its tool uses, and their values.
    pub(super) fn sync(&self, ui: &Ui<Msg>, model: &Model) {
        self.tool.get().set_text(model.tool().label());
        let shown = Shown::of(model.tool());
        let show = |ids: &[WidgetId], on: bool| ids.iter().for_each(|id| ui.set_visible(*id, on));
        let (brush, shape) = (model.brush(), model.shape());
        let tool = model.tool();

        number(&self.size.get(), f64::from(model.size()));
        number(&self.hard.get(), f64::from(brush.hardness));
        let antialias = match tool {
            Tool::Text => model.text_style().antialias,
            tool if tool.is_shape() => shape.antialias,
            _ => brush.antialias,
        };
        check(&self.aa.get(), antialias);
        select(&self.arrow.get(), &ARROWS, shape.arrow);
        select(&self.style.get(), &SHAPE_MODES, shape.mode);
        number(&self.radius.get(), f64::from(shape.radius));
        show(&[self.size_cap.get().id(), self.size.get().id()], shown.size);
        show(&[self.hard_cap.get().id(), self.hard.get().id()], shown.hardness);
        show(&[self.aa.get().id()], shown.antialias);
        show(&[self.arrow_cap.get().id(), self.arrow.get().id()], shown.arrow);
        show(&[self.style_cap.get().id(), self.style.get().id()], shown.style);
        show(&[self.radius_cap.get().id(), self.radius.get().id()], shown.radius);

        let mode = self.mode.get();
        if let Some(index) = CombineMode::ALL.iter().position(|m| *m == model.sel_mode()) {
            mode.select(index);
        }
        number(&self.tolerance.get(), f64::from(model.wand_tolerance()));
        check(&self.contiguous.get(), model.wand_contiguous());
        show(&[self.mode_caption.get().id(), mode.id()], shown.select_mode);
        show(
            &[
                self.tolerance_caption.get().id(),
                self.tolerance.get().id(),
                self.contiguous.get().id(),
            ],
            shown.wand,
        );

        let fill = model.fill();
        number(&self.fill_tol.get(), f64::from(fill.tolerance));
        select(&self.fill_area.get(), &AREAS, fill.area);
        select(&self.fill_sample.get(), &SAMPLES, fill.sample);
        show(
            &[
                self.fill_tol_cap.get().id(),
                self.fill_tol.get().id(),
                self.fill_area.get().id(),
                self.fill_sample.get().id(),
            ],
            shown.bucket,
        );

        let picker = model.picker();
        select(&self.pick_size.get(), &PICK_SIZES, picker.size);
        select(&self.pick_sample.get(), &SAMPLES, picker.sample);
        check(&self.pick_back.get(), picker.switch_back);
        show(
            &[
                self.pick_size.get().id(),
                self.pick_sample.get().id(),
                self.pick_back.get().id(),
            ],
            shown.picker,
        );

        select(&self.gradient.get(), &GRADIENTS, model.gradient().kind);
        show(&[self.gradient_cap.get().id(), self.gradient.get().id()], shown.gradient);

        let style = model.text_style();
        if let Some(index) = self.families.iter().position(|f| *f == style.family) {
            if self.family.get().selected() != index {
                self.family.get().select(index);
            }
        }
        number(&self.font_size.get(), f64::from(style.size));
        check(&self.bold.get(), style.bold);
        check(&self.italic.get(), style.italic);
        check(&self.underline.get(), style.underline);
        select(&self.align.get(), &ALIGNS, style.align);
        show(
            &[
                self.family.get().id(),
                self.font_size.get().id(),
                self.bold.get().id(),
                self.italic.get().id(),
                self.underline.get().id(),
                self.align.get().id(),
            ],
            shown.text,
        );
    }

    /// The named controls, for the UI probe.
    pub(super) fn probe_ids(&self) -> Vec<(&'static str, WidgetId)> {
        vec![
            ("size", self.size.get().id()),
            ("hardness", self.hard.get().id()),
            ("antialias", self.aa.get().id()),
            ("arrow", self.arrow.get().id()),
            ("shape_mode", self.style.get().id()),
            ("radius", self.radius.get().id()),
            ("sel_mode", self.mode.get().id()),
            ("wand_tolerance", self.tolerance.get().id()),
            ("fill_tolerance", self.fill_tol.get().id()),
            ("fill_area", self.fill_area.get().id()),
            ("fill_sample", self.fill_sample.get().id()),
            ("pick_size", self.pick_size.get().id()),
            ("pick_sample", self.pick_sample.get().id()),
            ("pick_back", self.pick_back.get().id()),
            ("gradient_kind", self.gradient.get().id()),
            ("font_family", self.family.get().id()),
            ("font_size", self.font_size.get().id()),
            ("bold", self.bold.get().id()),
            ("italic", self.italic.get().id()),
            ("underline", self.underline.get().id()),
            ("text_align", self.align.get().id()),
        ]
    }
}

/// The bar's row, bound to `options`.
pub(super) fn row(options: &Options) -> Layout<Msg> {
    let modes: Vec<&str> = CombineMode::ALL.iter().map(|m| m.label()).collect();
    let families: Vec<&str> = if options.families.is_empty() {
        vec!["(no fonts)"]
    } else {
        options.families.iter().map(String::as_str).collect()
    };
    arrange_row()
        .gap(8)
        .padding(6)
        .align(Align::Center)
        .children((
            label("Pencil").title().bind(&options.tool).width(140),
            label("Width:").bind(&options.size_cap).width(44),
            number_field(1.0, f64::from(MAX_BRUSH_WIDTH), 1.0)
                .on_change(|value| Msg::Size(value.clamp(1.0, f64::from(MAX_BRUSH_WIDTH)) as u32))
                .bind(&options.size)
                .width(72),
            label("Hardness:").bind(&options.hard_cap).width(64),
            number_field(0.0, 100.0, 5.0)
                .on_change(|value| Msg::Opt(OptMsg::Hardness(value.clamp(0.0, 100.0) as u8)))
                .bind(&options.hard)
                .width(72),
            checkbox("Antialiasing")
                .checked(true)
                .on_toggle(|on| Msg::Opt(OptMsg::Antialias(on)))
                .bind(&options.aa)
                .width(112),
        ))
        .children((
            label("Arrows:").bind(&options.arrow_cap).width(48),
            combo_box(&names(&ARROWS))
                .on_select(|index| Msg::Opt(OptMsg::Arrow(pick(&ARROWS, index))))
                .bind(&options.arrow)
                .width(80),
            label("Shape:").bind(&options.style_cap).width(44),
            combo_box(&names(&SHAPE_MODES))
                .on_select(|index| Msg::Opt(OptMsg::ShapeMode(pick(&SHAPE_MODES, index))))
                .bind(&options.style)
                .width(84),
            label("Radius:").bind(&options.radius_cap).width(48),
            number_field(0.0, 500.0, 1.0)
                .on_change(|value| Msg::Opt(OptMsg::Radius(value.clamp(0.0, 500.0) as u32)))
                .bind(&options.radius)
                .width(72),
        ))
        .children((
            label("Mode:").bind(&options.mode_caption).width(40),
            combo_box(&modes)
                .on_select(|index| Msg::SelectMode(CombineMode::ALL[index.min(4)]))
                .bind(&options.mode)
                .width(96),
            label("Tolerance %:")
                .bind(&options.tolerance_caption)
                .width(80),
            number_field(0.0, 100.0, 5.0)
                .on_change(|value| Msg::WandTolerance(value.clamp(0.0, 100.0) as u8))
                .bind(&options.tolerance)
                .width(72),
            checkbox("Contiguous")
                .checked(true)
                .on_toggle(Msg::WandContiguous)
                .bind(&options.contiguous)
                .width(110),
        ))
        .children((
            label("Tolerance %:").bind(&options.fill_tol_cap).width(80),
            number_field(0.0, 100.0, 5.0)
                .on_change(|value| Msg::Opt(OptMsg::FillTolerance(value.clamp(0.0, 100.0) as u8)))
                .bind(&options.fill_tol)
                .width(72),
            combo_box(&names(&AREAS))
                .on_select(|index| Msg::Opt(OptMsg::FillArea(pick(&AREAS, index))))
                .bind(&options.fill_area)
                .width(104),
            combo_box(&names(&SAMPLES))
                .on_select(|index| Msg::Opt(OptMsg::FillSample(pick(&SAMPLES, index))))
                .bind(&options.fill_sample)
                .width(76),
        ))
        .children((
            combo_box(&names(&PICK_SIZES))
                .on_select(|index| Msg::Opt(OptMsg::PickSize(pick(&PICK_SIZES, index))))
                .bind(&options.pick_size)
                .width(84),
            combo_box(&names(&SAMPLES))
                .on_select(|index| Msg::Opt(OptMsg::PickSample(pick(&SAMPLES, index))))
                .bind(&options.pick_sample)
                .width(76),
            checkbox("Switch back")
                .on_toggle(|on| Msg::Opt(OptMsg::PickSwitchBack(on)))
                .bind(&options.pick_back)
                .width(104),
        ))
        .children((
            label("Gradient:").bind(&options.gradient_cap).width(60),
            combo_box(&names(&GRADIENTS))
                .on_select(|index| Msg::Opt(OptMsg::GradientKind(pick(&GRADIENTS, index))))
                .bind(&options.gradient)
                .width(96),
        ))
        .children((
            combo_box(&families)
                .on_select(|index| Msg::Opt(OptMsg::Family(index)))
                .bind(&options.family)
                .width(150),
            number_field(4.0, 500.0, 1.0)
                .on_change(|value| Msg::Opt(OptMsg::FontSize(value.clamp(4.0, 500.0) as u32)))
                .bind(&options.font_size)
                .width(64),
            checkbox("B")
                .on_toggle(|on| Msg::Opt(OptMsg::Bold(on)))
                .bind(&options.bold)
                .width(40),
            checkbox("I")
                .on_toggle(|on| Msg::Opt(OptMsg::Italic(on)))
                .bind(&options.italic)
                .width(36),
            checkbox("U")
                .on_toggle(|on| Msg::Opt(OptMsg::Underline(on)))
                .bind(&options.underline)
                .width(40),
            combo_box(&names(&ALIGNS))
                .on_select(|index| Msg::Opt(OptMsg::Align(pick(&ALIGNS, index))))
                .bind(&options.align)
                .width(80),
        ))
}

#[cfg(test)]
mod tests;
