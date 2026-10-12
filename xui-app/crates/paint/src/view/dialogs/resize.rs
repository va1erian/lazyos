#![forbid(unsafe_code)]

//! Image > Resize: scale the picture by a percentage or to a pixel size, with
//! the aspect ratio kept or not, by nearest-neighbour or bilinear sampling.
//!
//! Whichever field the user edits decides the mode (a percentage selects "By
//! percent", a side selects "By pixels") and the others follow it, so the
//! radio and the numbers cannot disagree. The summary line under the fields
//! shows the new size, or what is wrong with the input, before OK is pressed.

use std::cell::Cell;

use xui_core::app::Ui;
use xui_core::arrange::{
    GroupBuild, Handle, Layout, LayoutExt, checkbox, column, group, label, radio_group, row,
};
use xui_core::backend::WidgetId;
use xui_core::layout::Align;
use xui_core::widget::{Button, CheckBox, Edit, GroupBox, HasText, Label, RadioGroup};

use super::{ROW, button_row, field_row, parse_side, scaled, show_radio};
use crate::model::{MAX_SIDE, Resample};
use crate::view::Msg;

/// A message from the Resize dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResizeMsg {
    /// The percentage field changed.
    Percent,
    /// The width field changed.
    Width,
    /// The height field changed.
    Height,
    /// The By percent / By pixels choice changed.
    Mode,
    /// "Maintain aspect ratio" was toggled.
    Aspect,
    /// OK.
    Ok,
    /// Cancel.
    Cancel,
}

/// The mode radio's indexes.
const BY_PERCENT: usize = 0;
const BY_PIXELS: usize = 1;

/// The Resize dialog's widgets.
pub(in crate::view) struct ResizeDialog {
    frame: Handle<GroupBox<Msg>>,
    mode: Handle<RadioGroup<Msg>>,
    percent: Handle<Edit<Msg>>,
    width: Handle<Edit<Msg>>,
    height: Handle<Edit<Msg>>,
    aspect: Handle<CheckBox<Msg>>,
    resample: Handle<RadioGroup<Msg>>,
    summary: Handle<Label<Msg>>,
    ok: Handle<Button<Msg>>,
    cancel: Handle<Button<Msg>>,
    /// The picture's size when the dialog opened: what the fields scale.
    base: Cell<(u32, u32)>,
}

impl ResizeDialog {
    pub(in crate::view) fn new() -> ResizeDialog {
        ResizeDialog {
            frame: Handle::new(),
            mode: Handle::new(),
            percent: Handle::new(),
            width: Handle::new(),
            height: Handle::new(),
            aspect: Handle::new(),
            resample: Handle::new(),
            summary: Handle::new(),
            ok: Handle::new(),
            cancel: Handle::new(),
            base: Cell::new((1, 1)),
        }
    }

    /// The frame and its content, for the layout to place over the canvas.
    pub(in crate::view) fn layout(&self) -> GroupBuild<Msg> {
        let content: Layout<Msg> = column().gap(8).padding(10).children((
            label("Resize by").caption().fixed(20.0),
            radio_group(&["Percentage", "Absolute size"])
                .on_select(|_| Msg::Resize(ResizeMsg::Mode))
                .bind(&self.mode)
                .fixed(52.0),
            field_row(
                "Percentage:",
                "%",
                &self.percent,
                Msg::Resize(ResizeMsg::Percent),
                90.0,
            ),
            field_row(
                "Width:",
                "px",
                &self.width,
                Msg::Resize(ResizeMsg::Width),
                90.0,
            ),
            field_row(
                "Height:",
                "px",
                &self.height,
                Msg::Resize(ResizeMsg::Height),
                90.0,
            ),
            checkbox("Maintain aspect ratio")
                .checked(true)
                .on_toggle(|_| Msg::Resize(ResizeMsg::Aspect))
                .bind(&self.aspect)
                .fixed(ROW - 4.0),
            row()
                .gap(8)
                .align(Align::Start)
                .children((
                    label("Resampling:").width(90),
                    radio_group(&["Bilinear (smooth)", "Nearest neighbour (hard edges)"])
                        .bind(&self.resample)
                        .fill(1),
                ))
                .fixed(56.0),
            label("").bind(&self.summary).fixed(ROW - 6.0),
            button_row(
                "Scales every layer.",
                (Msg::Resize(ResizeMsg::Ok), &self.ok),
                (Msg::Resize(ResizeMsg::Cancel), &self.cancel),
            ),
        ));
        group("Resize", content).bind(&self.frame)
    }

    /// Hides the frame; the app calls it once after mounting.
    pub(in crate::view) fn hide_initially(&self, ui: &Ui<Msg>) {
        self.close(ui);
    }

    /// Whether the dialog is showing.
    pub(in crate::view) fn is_open(&self, ui: &Ui<Msg>) -> bool {
        self.frame
            .try_get()
            .is_some_and(|frame| ui.is_visible(frame.id()))
    }

    /// Shows the dialog for a picture of `size`: 100%, the current sides,
    /// aspect kept, bilinear.
    pub(in crate::view) fn open(&self, ui: &Ui<Msg>, size: (u32, u32)) {
        self.base.set(size);
        self.mode.get().select(BY_PERCENT);
        self.resample.get().select(0);
        self.aspect.get().set_checked(true);
        self.percent.get().set_text("100");
        self.width.get().set_text(&size.0.to_string());
        self.height.get().set_text(&size.1.to_string());
        self.refresh_summary();
        show_radio(ui, &self.mode, true);
        show_radio(ui, &self.resample, true);
        ui.set_visible(self.frame.get().id(), true);
        ui.relayout();
        self.percent.get().focus();
    }

    /// Hides the dialog.
    pub(in crate::view) fn close(&self, ui: &Ui<Msg>) {
        show_radio(ui, &self.mode, false);
        show_radio(ui, &self.resample, false);
        ui.set_visible(self.frame.get().id(), false);
    }

    /// A field or choice changed: bring the other fields and the summary in
    /// step with it.
    pub(in crate::view) fn edited(&self, msg: ResizeMsg) {
        let (base_w, base_h) = self.base.get();
        let aspect = self.aspect.get().is_checked();
        match msg {
            ResizeMsg::Percent => {
                self.mode.get().select(BY_PERCENT);
                self.show_percent_sides();
            }
            ResizeMsg::Width => {
                self.mode.get().select(BY_PIXELS);
                if let Ok(width) = parse_side(&self.width.get().text(), "Width") {
                    if aspect {
                        let height = scaled(width, base_h, base_w);
                        self.height.get().set_text(&height.to_string());
                    }
                    self.show_percent_of(width, base_w);
                }
            }
            ResizeMsg::Height => {
                self.mode.get().select(BY_PIXELS);
                if let Ok(height) = parse_side(&self.height.get().text(), "Height") {
                    if aspect {
                        let width = scaled(height, base_w, base_h);
                        self.width.get().set_text(&width.to_string());
                    }
                    self.show_percent_of(height, base_h);
                }
            }
            ResizeMsg::Mode if self.mode.get().selected() == BY_PERCENT => {
                self.show_percent_sides();
            }
            ResizeMsg::Aspect if aspect && self.mode.get().selected() == BY_PIXELS => {
                if let Ok(width) = parse_side(&self.width.get().text(), "Width") {
                    let height = scaled(width, base_h, base_w);
                    self.height.get().set_text(&height.to_string());
                }
            }
            _ => {}
        }
        self.refresh_summary();
    }

    /// Writes the sides the percentage field gives into the side fields.
    fn show_percent_sides(&self) {
        if let Some(percent) = self.percent_value() {
            let (w, h) = self.sides_at(percent);
            self.width.get().set_text(&w.to_string());
            self.height.get().set_text(&h.to_string());
        }
    }

    /// Writes into the percentage field what `part` of `whole` is.
    fn show_percent_of(&self, part: u32, whole: u32) {
        let percent = scaled(part, 100, whole);
        self.percent.get().set_text(&percent.to_string());
    }

    /// The percentage typed, if it is a whole number above zero.
    fn percent_value(&self) -> Option<u32> {
        let text = self.percent.get().text();
        let text = text.trim();
        let digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
        digits
            .then(|| text.parse().unwrap_or(u32::MAX))
            .filter(|&p| p > 0)
    }

    /// The sides a picture of the dialog's base size has at `percent`.
    fn sides_at(&self, percent: u32) -> (u32, u32) {
        let (w, h) = self.base.get();
        (scaled(w, percent, 100), scaled(h, percent, 100))
    }

    /// Shows the new size, or what is wrong, under the fields.
    fn refresh_summary(&self) {
        let text = match self.read() {
            Ok((w, h, _)) => {
                let (old_w, old_h) = self.base.get();
                format!("{old_w} x {old_h}  becomes  {w} x {h}")
            }
            Err(error) => error,
        };
        self.summary.get().set_text(&text);
    }

    /// The size and sampling the controls ask for, or why they cannot be used.
    pub(in crate::view) fn read(&self) -> Result<(u32, u32, Resample), String> {
        let how = if self.resample.get().selected() == 0 {
            Resample::Bilinear
        } else {
            Resample::Nearest
        };
        let (width, height) = if self.mode.get().selected() == BY_PERCENT {
            let percent = self
                .percent_value()
                .ok_or("Percentage must be a whole number above 0")?;
            let (w, h) = self.sides_at(percent);
            if w > MAX_SIDE || h > MAX_SIDE {
                return Err(format!(
                    "{percent}% would make {w} x {h}; the largest side is {MAX_SIDE} pixels"
                ));
            }
            (w, h)
        } else {
            (
                parse_side(&self.width.get().text(), "Width")?,
                parse_side(&self.height.get().text(), "Height")?,
            )
        };
        Ok((width, height, how))
    }

    /// The controls' nodes with their probe names.
    pub(in crate::view) fn ids(&self) -> Option<[(&'static str, WidgetId); 8]> {
        Some([
            ("resize_percent", self.percent.try_get()?.id()),
            ("resize_width", self.width.try_get()?.id()),
            ("resize_height", self.height.try_get()?.id()),
            ("resize_aspect", self.aspect.try_get()?.id()),
            ("resize_mode", self.mode.try_get()?.ids().first().copied()?),
            (
                "resize_resample",
                self.resample.try_get()?.ids().first().copied()?,
            ),
            ("resize_ok", self.ok.try_get()?.id()),
            ("resize_cancel", self.cancel.try_get()?.id()),
        ])
    }
}
