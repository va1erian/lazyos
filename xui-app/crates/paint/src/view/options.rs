#![forbid(unsafe_code)]

//! The tool options bar: the active tool's name and the controls that tool
//! offers, in one row under the menu bar.
//!
//! Today that is the brush size, for the tools that use one. Later phases add
//! their controls (antialiasing, fill mode, tolerance, font, ...) the same way:
//! build the widget in [`row`], keep its handle in [`Options`], and show or hide
//! it in [`Options::sync`] by the tool, so a tool with no controls leaves the
//! bar with just its name.

use xui_core::app::Ui;
use xui_core::arrange::{
    Handle, Layout, LayoutExt, checkbox, combo_box, label, number_field, row as arrange_row,
};
use xui_core::layout::Align;
use xui_core::widget::{CheckBox, ComboBox, HasText, Label, NumberField};

use super::Msg;
use crate::model::{CombineMode, Model, SIZES, Tool};

/// Whether `tool` paints with the brush size.
pub fn uses_size(tool: Tool) -> bool {
    matches!(
        tool,
        Tool::Brush | Tool::Eraser | Tool::Line | Tool::Rectangle | Tool::Ellipse
    )
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

/// The bar's widgets, kept to update them from the model.
pub(super) struct Options {
    tool: Handle<Label<Msg>>,
    caption: Handle<Label<Msg>>,
    size: Handle<ComboBox<Msg>>,
    mode_caption: Handle<Label<Msg>>,
    mode: Handle<ComboBox<Msg>>,
    tolerance_caption: Handle<Label<Msg>>,
    tolerance: Handle<NumberField<Msg>>,
    contiguous: Handle<CheckBox<Msg>>,
}

impl Options {
    pub(super) fn new() -> Options {
        Options {
            tool: Handle::new(),
            caption: Handle::new(),
            size: Handle::new(),
            mode_caption: Handle::new(),
            mode: Handle::new(),
            tolerance_caption: Handle::new(),
            tolerance: Handle::new(),
            contiguous: Handle::new(),
        }
    }

    /// Shows `model`'s tool name and size, and the controls its tool uses.
    pub(super) fn sync(&self, ui: &Ui<Msg>, model: &Model) {
        self.tool.get().set_text(model.tool().label());
        let visible = uses_size(model.tool());
        let combo = self.size.get();
        if let Some(index) = SIZES.iter().position(|size| *size == model.size()) {
            combo.select(index);
        }
        ui.set_visible(self.caption.get().id(), visible);
        ui.set_visible(combo.id(), visible);

        let selector = model.tool().is_selector();
        let wand = model.tool() == Tool::MagicWand;
        let mode = self.mode.get();
        if let Some(index) = CombineMode::ALL.iter().position(|m| *m == model.sel_mode()) {
            mode.select(index);
        }
        ui.set_visible(self.mode_caption.get().id(), selector);
        ui.set_visible(mode.id(), selector);
        let tolerance = self.tolerance.get();
        if tolerance.value() as u8 != model.wand_tolerance() {
            tolerance.set_value(f64::from(model.wand_tolerance()));
        }
        let contiguous = self.contiguous.get();
        if contiguous.is_checked() != model.wand_contiguous() {
            contiguous.set_checked(model.wand_contiguous());
        }
        for id in [
            self.tolerance_caption.get().id(),
            tolerance.id(),
            contiguous.id(),
        ] {
            ui.set_visible(id, wand);
        }
    }

    /// The size control, for tests and the UI probe.
    pub(super) fn size_id(&self) -> xui_core::backend::WidgetId {
        self.size.get().id()
    }

    /// The selection mode control, for the UI probe.
    pub(super) fn mode_id(&self) -> xui_core::backend::WidgetId {
        self.mode.get().id()
    }

    /// The Magic Wand tolerance control, for the UI probe.
    pub(super) fn tolerance_id(&self) -> xui_core::backend::WidgetId {
        self.tolerance.get().id()
    }
}

/// The bar's row, bound to `options`.
pub(super) fn row(options: &Options) -> Layout<Msg> {
    let labels: Vec<String> = SIZES.iter().map(|size| format!("{size} px")).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let modes: Vec<&str> = CombineMode::ALL.iter().map(|m| m.label()).collect();
    arrange_row()
        .gap(10)
        .padding(6)
        .align(Align::Center)
        .children((
            label("Pencil").title().bind(&options.tool).width(180),
            label("Size:").bind(&options.caption).width(36),
            combo_box(&labels)
                .on_select(|index| Msg::Size(SIZES[index.min(SIZES.len() - 1)]))
                .bind(&options.size)
                .width(84),
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
}

#[cfg(test)]
mod tests;
