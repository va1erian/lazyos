#![forbid(unsafe_code)]

//! Adjustments > Brightness/Contrast and Hue/Saturation: a strip of sliders
//! under the canvas, so the picture stays in view while they move.
//!
//! Every slider move raises [`AdjustMsg::Changed`]; the app reads the values
//! back ([`AdjustPanel::adjustment`]) and hands the canvas a screen-only
//! preview. Nothing reaches the document until OK, which applies the
//! adjustment to the active layer as one history entry; Cancel drops the
//! preview.

use xui_core::app::Ui;
use xui_core::arrange::{GroupBuild, Handle, Layout, LayoutExt, column, group, label, row, slider};
use xui_core::backend::WidgetId;
use xui_core::layout::Align;
use xui_core::widget::{Button, GroupBox, HasText, Label, Slider};

use super::button_row;
use crate::model::Adjustment;
use crate::view::Msg;

/// Which adjustment a panel edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdjustKind {
    /// Brightness and contrast.
    BrightnessContrast,
    /// Hue, saturation and lightness.
    HueSaturation,
}

/// A message from an adjustment panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdjustMsg {
    /// A slider moved; the app reads them back.
    Changed,
    /// OK.
    Ok,
    /// Cancel.
    Cancel,
}

/// One slider: its caption, range, neutral value and the unit its value shows.
struct Row {
    caption: &'static str,
    min: i32,
    max: i32,
    neutral: i32,
    unit: &'static str,
}

impl AdjustKind {
    fn title(self) -> &'static str {
        match self {
            AdjustKind::BrightnessContrast => "Brightness / Contrast",
            AdjustKind::HueSaturation => "Hue / Saturation",
        }
    }

    fn rows(self) -> &'static [Row] {
        match self {
            AdjustKind::BrightnessContrast => &[
                Row {
                    caption: "Brightness:",
                    min: -100,
                    max: 100,
                    neutral: 0,
                    unit: "",
                },
                Row {
                    caption: "Contrast:",
                    min: -100,
                    max: 100,
                    neutral: 0,
                    unit: "",
                },
            ],
            AdjustKind::HueSaturation => &[
                Row {
                    caption: "Hue:",
                    min: -180,
                    max: 180,
                    neutral: 0,
                    unit: "\u{b0}",
                },
                Row {
                    caption: "Saturation:",
                    min: 0,
                    max: 200,
                    neutral: 100,
                    unit: "%",
                },
                Row {
                    caption: "Lightness:",
                    min: -100,
                    max: 100,
                    neutral: 0,
                    unit: "",
                },
            ],
        }
    }

    /// The height the strip needs, in design units: the frame's title and
    /// padding, one row per slider and the button row.
    pub(in crate::view) fn height(self) -> f32 {
        64.0 + 36.0 * self.rows().len() as f32 + 34.0
    }
}

/// The text a slider's value shows: signed where it is an offset.
fn show(value: i32, row: &Row) -> String {
    if row.min < 0 && value > 0 {
        format!("+{value}{}", row.unit)
    } else {
        format!("{value}{}", row.unit)
    }
}

/// One adjustment's strip of sliders.
pub(in crate::view) struct AdjustPanel {
    kind: AdjustKind,
    frame: Handle<GroupBox<Msg>>,
    sliders: Vec<Handle<Slider<Msg>>>,
    values: Vec<Handle<Label<Msg>>>,
    ok: Handle<Button<Msg>>,
    cancel: Handle<Button<Msg>>,
}

impl AdjustPanel {
    pub(in crate::view) fn new(kind: AdjustKind) -> AdjustPanel {
        let count = kind.rows().len();
        AdjustPanel {
            kind,
            frame: Handle::new(),
            sliders: (0..count).map(|_| Handle::new()).collect(),
            values: (0..count).map(|_| Handle::new()).collect(),
            ok: Handle::new(),
            cancel: Handle::new(),
        }
    }

    /// Which adjustment this edits.
    pub(in crate::view) fn kind(&self) -> AdjustKind {
        self.kind
    }

    /// The frame and its content, for the layout to place under the canvas.
    pub(in crate::view) fn layout(&self) -> GroupBuild<Msg> {
        let kind = self.kind;
        let mut children: Vec<_> = kind
            .rows()
            .iter()
            .enumerate()
            .map(|(index, spec)| {
                row()
                    .gap(8)
                    .align(Align::Center)
                    .children((
                        label(spec.caption).width(84),
                        slider(f64::from(spec.min), f64::from(spec.max))
                            .on_change(move |_| Msg::Adjust(kind, AdjustMsg::Changed))
                            .bind(&self.sliders[index])
                            .fill(1),
                        label(show(spec.neutral, spec))
                            .bind(&self.values[index])
                            .width(56),
                    ))
                    .fixed(28.0)
            })
            .collect();
        children.push(button_row(
            "A preview until OK.",
            (Msg::Adjust(kind, AdjustMsg::Ok), &self.ok),
            (Msg::Adjust(kind, AdjustMsg::Cancel), &self.cancel),
        ));
        let content: Layout<Msg> = column().gap(8).padding(8).children(children);
        group(kind.title(), content).bind(&self.frame)
    }

    /// Hides the frame; the app calls it once after mounting.
    pub(in crate::view) fn hide_initially(&self, ui: &Ui<Msg>) {
        ui.set_visible(self.frame.get().id(), false);
    }

    /// Whether the strip is showing.
    pub(in crate::view) fn is_open(&self, ui: &Ui<Msg>) -> bool {
        self.frame
            .try_get()
            .is_some_and(|frame| ui.is_visible(frame.id()))
    }

    /// Shows the strip with every slider neutral.
    pub(in crate::view) fn open(&self, ui: &Ui<Msg>) {
        self.reset();
        ui.set_visible(self.frame.get().id(), true);
        ui.relayout();
    }

    /// Hides the strip.
    pub(in crate::view) fn close(&self, ui: &Ui<Msg>) {
        ui.set_visible(self.frame.get().id(), false);
    }

    /// Puts every slider on its neutral value.
    pub(in crate::view) fn reset(&self) {
        for (index, spec) in self.kind.rows().iter().enumerate() {
            self.sliders[index].get().set_value(f64::from(spec.neutral));
        }
        self.show_values();
    }

    /// Refreshes the value labels from the sliders.
    pub(in crate::view) fn show_values(&self) {
        for (index, spec) in self.kind.rows().iter().enumerate() {
            self.values[index]
                .get()
                .set_text(&show(self.value(index), spec));
        }
    }

    /// Slider `index` as a whole number in its range.
    fn value(&self, index: usize) -> i32 {
        let spec = &self.kind.rows()[index];
        let raw = self.sliders[index].get().value().round();
        (raw as i32).clamp(spec.min, spec.max)
    }

    /// The adjustment the sliders ask for.
    pub(in crate::view) fn adjustment(&self) -> Adjustment {
        match self.kind {
            AdjustKind::BrightnessContrast => Adjustment::BrightnessContrast {
                brightness: self.value(0),
                contrast: self.value(1),
            },
            AdjustKind::HueSaturation => Adjustment::HueSaturation {
                hue: self.value(0),
                saturation: self.value(1),
                lightness: self.value(2),
            },
        }
    }

    /// The controls' nodes with their probe names.
    pub(in crate::view) fn ids(&self) -> Option<Vec<(String, WidgetId)>> {
        let mut ids = vec![
            ("adjust_ok".to_string(), self.ok.try_get()?.id()),
            ("adjust_cancel".to_string(), self.cancel.try_get()?.id()),
        ];
        for (index, slider) in self.sliders.iter().enumerate() {
            ids.push((format!("adjust_slider:{index}"), slider.try_get()?.id()));
        }
        Some(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_are_signed_and_percentages_are_not() {
        let hue = &AdjustKind::HueSaturation.rows()[0];
        assert_eq!(show(45, hue), "+45\u{b0}");
        assert_eq!(show(-45, hue), "-45\u{b0}");
        assert_eq!(show(0, hue), "0\u{b0}");
        let saturation = &AdjustKind::HueSaturation.rows()[1];
        assert_eq!(show(150, saturation), "150%");
    }
}
