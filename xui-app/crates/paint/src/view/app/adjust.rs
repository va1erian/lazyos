#![forbid(unsafe_code)]

//! The Adjustments menu on the app: the instant ones, and the two with a
//! slider strip and a live preview.
//!
//! While a strip is open the canvas shows the adjustment on its screen image
//! only ([`AdjustPreview`]); the document and its history are untouched until
//! OK, which applies the adjustment to the active layer (through the
//! selection) as one history entry. Cancel drops the preview.

use xui_core::app::Ui;

use super::{Msg, PaintApp};
use crate::model::Adjustment;
use crate::view::dialogs::AdjustPanel;
use crate::view::{AdjustKind, AdjustMsg, AdjustPreview, Command, LayerStack};

impl PaintApp {
    /// Runs an Adjustments command; `false` for any other.
    pub(super) fn adjust_command(&mut self, ui: &Ui<Msg>, command: Command) -> bool {
        let instant = match command {
            Command::InvertColors => Adjustment::Invert,
            Command::BlackAndWhite => Adjustment::BlackAndWhite,
            Command::Sepia => Adjustment::Sepia,
            Command::BrightnessContrast => {
                return self.open_adjust(ui, AdjustKind::BrightnessContrast);
            }
            Command::HueSaturation => return self.open_adjust(ui, AdjustKind::HueSaturation),
            _ => return false,
        };
        self.message = Some(match self.model.adjust(instant) {
            Ok(()) => command.label().to_string(),
            Err(error) => format!("{} failed: {error}", command.label()),
        });
        true
    }

    /// The panel for `kind`.
    fn panel(&self, kind: AdjustKind) -> &AdjustPanel {
        match kind {
            AdjustKind::BrightnessContrast => &self.frames.brightness,
            AdjustKind::HueSaturation => &self.frames.hue,
        }
    }

    /// Opens the strip for `kind` with neutral sliders.
    fn open_adjust(&mut self, ui: &Ui<Msg>, kind: AdjustKind) -> bool {
        // The active layer's turn is previewed against the layers as they are
        // now; the dialog is modal, so they stay that way until it closes.
        self.adjust_stack = LayerStack::of(self.model.document());
        self.panel(kind).open(ui);
        true
    }

    /// Shows the sliders' adjustment on the canvas, through the selection.
    fn preview_adjustment(&self, kind: AdjustKind) {
        let panel = self.panel(kind);
        panel.show_values();
        // A hidden active layer has nothing to show: OK changes it unseen.
        let preview = self.adjust_stack.clone().map(|stack| {
            AdjustPreview::new(panel.adjustment(), self.model.selection().cloned()).on_layer(stack)
        });
        self.canvas.set_adjust_preview(preview);
    }

    /// A message from an adjustment strip.
    pub(super) fn adjust_msg(&mut self, ui: &Ui<Msg>, kind: AdjustKind, msg: AdjustMsg) {
        match msg {
            AdjustMsg::Changed => self.preview_adjustment(kind),
            AdjustMsg::Cancel => self.close_adjust(ui, kind),
            AdjustMsg::Ok => {
                let adjustment = self.panel(kind).adjustment();
                self.close_adjust(ui, kind);
                self.message = Some(match self.model.adjust(adjustment) {
                    Ok(()) => adjustment.name().to_string(),
                    Err(error) => format!("{} failed: {error}", adjustment.name()),
                });
            }
        }
    }

    /// Hides the strip and the preview, and gives the canvas the keyboard.
    fn close_adjust(&mut self, ui: &Ui<Msg>, kind: AdjustKind) {
        self.panel(kind).close(ui);
        self.canvas.set_adjust_preview(None);
        self.adjust_stack = None;
        ui.focus(self.canvas.id());
    }
}
