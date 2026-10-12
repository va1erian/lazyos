#![forbid(unsafe_code)]

//! The Image and Adjustments dialogs: Resize, Canvas Size, Brightness/Contrast
//! and Hue/Saturation.
//!
//! Each is a titled frame in the window, like the colour editor and Layer
//! Properties (xui gap: G17 — `Dialog` holds only text and one entry, so a
//! dialog with several fields, a radio group or a slider cannot be one). The
//! Resize and Canvas Size frames take the canvas's place; the adjustment
//! strips sit under it so the picture stays in view for the preview.
//!
//! The controls raise payload-free messages ([`Msg`] is `Copy`); the app reads
//! the controls back with each dialog's `read`, which validates the input and
//! returns a message fit for the status bar when it is not usable.

mod adjust;
mod canvas_size;
mod prompts;
mod resize;

pub(super) use adjust::AdjustPanel;
pub use adjust::{AdjustKind, AdjustMsg};
pub(super) use canvas_size::CanvasSizeDialog;
pub use canvas_size::CanvasSizeMsg;
pub(super) use prompts::{FlattenPrompt, UnsavedPrompt};
pub(super) use resize::ResizeDialog;
pub use resize::ResizeMsg;

use xui_core::app::Ui;
use xui_core::arrange::GroupBuild;
use xui_core::backend::WidgetId;

/// The four frames, mounted by the window layout: Resize and Canvas Size over
/// the canvas, the two adjustment strips under it.
pub(super) struct Frames {
    pub resize: ResizeDialog,
    pub canvas_size: CanvasSizeDialog,
    pub brightness: AdjustPanel,
    pub hue: AdjustPanel,
}

impl Frames {
    pub(super) fn new() -> Frames {
        Frames {
            resize: ResizeDialog::new(),
            canvas_size: CanvasSizeDialog::new(),
            brightness: AdjustPanel::new(AdjustKind::BrightnessContrast),
            hue: AdjustPanel::new(AdjustKind::HueSaturation),
        }
    }

    /// Hides every frame; the app calls it once after mounting.
    pub(super) fn hide_initially(&self, ui: &Ui<Msg>) {
        self.resize.hide_initially(ui);
        self.canvas_size.hide_initially(ui);
        self.brightness.hide_initially(ui);
        self.hue.hide_initially(ui);
    }

    /// Whether any frame is showing.
    pub(super) fn is_open(&self, ui: &Ui<Msg>) -> bool {
        self.resize.is_open(ui)
            || self.canvas_size.is_open(ui)
            || self.brightness.is_open(ui)
            || self.hue.is_open(ui)
    }

    /// The messages Enter and Esc raise while a frame is open: OK and Cancel.
    pub(super) fn keys(&self, ui: &Ui<Msg>) -> Option<(Msg, Msg)> {
        if self.resize.is_open(ui) {
            return Some((Msg::Resize(ResizeMsg::Ok), Msg::Resize(ResizeMsg::Cancel)));
        }
        if self.canvas_size.is_open(ui) {
            return Some((
                Msg::CanvasSize(CanvasSizeMsg::Ok),
                Msg::CanvasSize(CanvasSizeMsg::Cancel),
            ));
        }
        [&self.brightness, &self.hue]
            .into_iter()
            .find(|panel| panel.is_open(ui))
            .map(|panel| {
                let kind = panel.kind();
                (
                    Msg::Adjust(kind, AdjustMsg::Ok),
                    Msg::Adjust(kind, AdjustMsg::Cancel),
                )
            })
    }

    /// The Resize and Canvas Size layouts, for the stack over the canvas.
    pub(super) fn over_canvas(&self) -> (GroupBuild<Msg>, GroupBuild<Msg>) {
        (self.resize.layout(), self.canvas_size.layout())
    }

    /// The probe names of whichever frame is open.
    pub(super) fn ids(&self, ui: &Ui<Msg>) -> Vec<(String, WidgetId)> {
        let mut ids = Vec::new();
        if self.resize.is_open(ui)
            && let Some(found) = self.resize.ids()
        {
            ids.extend(found.map(|(name, id)| (name.to_string(), id)));
        }
        if self.canvas_size.is_open(ui) {
            ids.extend(self.canvas_size.ids().unwrap_or_default());
        }
        for panel in [&self.brightness, &self.hue] {
            if panel.is_open(ui) {
                ids.extend(panel.ids().unwrap_or_default());
            }
        }
        ids
    }
}

use xui_core::arrange::{Entry, Handle, LayoutExt, button, edit, label, row};
use xui_core::layout::Align;
use xui_core::widget::{Button, Edit, RadioGroup};

use super::Msg;
use crate::model::{MAX_SIDE, MIN_SIDE};

/// Shows or hides the options of a radio group after its first.
///
/// A layout hides the content of a hidden frame by each widget's own id, and a
/// radio group's id is its first option's, so the others would stay on screen
/// (and keep catching clicks) over the canvas the frame covers. The first
/// option follows the frame; this does the rest.
fn show_radio(ui: &Ui<Msg>, group: &Handle<RadioGroup<Msg>>, shown: bool) {
    if let Some(group) = group.try_get() {
        for id in group.ids().into_iter().skip(1) {
            ui.set_visible(id, shown);
        }
    }
}

/// The height of one control row, in design units.
const ROW: f32 = 30.0;

/// A caption, a text field raising `changed`, and a unit after it.
fn field_row(
    caption: &str,
    unit: &str,
    field: &Handle<Edit<Msg>>,
    changed: Msg,
    caption_width: f32,
) -> Entry<Msg> {
    row()
        .gap(8)
        .align(Align::Center)
        .children((
            label(caption).width(caption_width),
            edit().on_change(move |_| changed).bind(field).width(90),
            label(unit).width(36),
        ))
        .fixed(ROW)
}

/// The bottom row of a dialog: a hint at the left, OK and Cancel at the right.
fn button_row(
    hint: &str,
    ok: (Msg, &Handle<Button<Msg>>),
    cancel: (Msg, &Handle<Button<Msg>>),
) -> Entry<Msg> {
    row()
        .gap(8)
        .align(Align::Center)
        .children((
            label(hint).caption().fill(1),
            button("OK").on_click(ok.0).primary().bind(ok.1).width(80),
            button("Cancel").on_click(cancel.0).bind(cancel.1).width(80),
        ))
        .fixed(34.0)
}

/// A whole number of pixels from a text field: `what` names the field in the
/// message when the text is empty, not a number or out of range.
pub(super) fn parse_side(text: &str, what: &str) -> Result<u32, String> {
    let text = text.trim();
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("{what} must be a whole number of pixels"));
    }
    // A value too long for u32 is far out of range anyway.
    let value: u32 = text.parse().unwrap_or(u32::MAX);
    if (MIN_SIDE..=MAX_SIDE).contains(&value) {
        Ok(value)
    } else {
        Err(format!("{what} must be {MIN_SIDE}-{MAX_SIDE} pixels"))
    }
}

/// `value` scaled by `numerator / denominator`, rounded, at least 1 and
/// saturating: the other side of a picture resized with its aspect ratio kept.
pub(super) fn scaled(value: u32, numerator: u32, denominator: u32) -> u32 {
    let denominator = u64::from(denominator.max(1));
    let scaled = (u64::from(value) * u64::from(numerator) + denominator / 2) / denominator;
    scaled.clamp(1, u64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sides_are_whole_numbers_in_range() {
        assert_eq!(parse_side(" 640 ", "Width"), Ok(640));
        assert_eq!(parse_side("16384", "Width"), Ok(16384));
        for bad in ["", "abc", "-5", "1.5", "0", "16385", "99999999999999999999"] {
            let error = parse_side(bad, "Width").unwrap_err();
            assert!(error.starts_with("Width must be"), "{bad}: {error}");
        }
    }

    #[test]
    fn scaling_rounds_and_never_reaches_zero() {
        assert_eq!(scaled(240, 640, 320), 480);
        assert_eq!(scaled(1, 1, 100), 1);
        assert_eq!(scaled(3, 1, 2), 2);
        assert_eq!(scaled(10, 5, 0), 50);
    }
}
