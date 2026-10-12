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
use xui_core::arrange::{Handle, Layout, LayoutExt, combo_box, label, row as arrange_row};
use xui_core::layout::Align;
use xui_core::widget::{ComboBox, HasText, Label};

use super::Msg;
use crate::model::{Model, SIZES, Tool};

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
}

impl Options {
    pub(super) fn new() -> Options {
        Options {
            tool: Handle::new(),
            caption: Handle::new(),
            size: Handle::new(),
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
    }

    /// The size control, for tests and the UI probe.
    pub(super) fn size_id(&self) -> xui_core::backend::WidgetId {
        self.size.get().id()
    }
}

/// The bar's row, bound to `options`.
pub(super) fn row(options: &Options) -> Layout<Msg> {
    let labels: Vec<String> = SIZES.iter().map(|size| format!("{size} px")).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    arrange_row()
        .gap(10)
        .padding(6)
        .align(Align::Center)
        .children((
            label("Pencil").title().bind(&options.tool).width(110),
            label("Size:").bind(&options.caption).width(36),
            combo_box(&labels)
                .on_select(|index| Msg::Size(SIZES[index.min(SIZES.len() - 1)]))
                .bind(&options.size)
                .width(84),
        ))
}

#[cfg(test)]
mod tests;
