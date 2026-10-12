#![forbid(unsafe_code)]

//! Colour editing: the editor's open/live/done/cancel flow and the reset.

use xui_core::app::Ui;

use super::{Msg, PaintApp};
use crate::model::{Pixel, Side, WHITE};

/// The primary colour `D` restores.
pub(super) const DEFAULT_PRIMARY: Pixel = [0, 0, 0, 255];
/// The secondary colour `D` restores.
pub(super) const DEFAULT_SECONDARY: Pixel = WHITE;

impl PaintApp {
    /// Opens the editor on `side`'s colour, remembering it for Cancel. The
    /// canvas is hidden while the editor takes its place.
    pub(super) fn edit_color(&mut self, ui: &Ui<Msg>, side: Side) {
        let original = match side {
            Side::Primary => self.model.primary(),
            Side::Secondary => self.model.secondary(),
        };
        self.editing = Some((side, original));
        self.canvas.set_visible(false);
        self.editor.open(ui, side, original);
    }

    /// The editor's panel changed the colour: apply it live, keeping the alpha
    /// the colour has now (the panel is RGB only).
    pub(super) fn color_editing(&mut self, edited: Pixel) {
        let Some((side, _)) = self.editing else {
            return;
        };
        let alpha = self.edited_color(side)[3];
        self.set_edited(side, [edited[0], edited[1], edited[2], alpha]);
    }

    /// The alpha slider moved: apply it live.
    pub(super) fn color_alpha(&mut self, alpha: u8) {
        let Some((side, _)) = self.editing else {
            return;
        };
        let color = self.edited_color(side);
        self.set_edited(side, [color[0], color[1], color[2], alpha]);
        self.editor.set_alpha(alpha);
    }

    fn edited_color(&self, side: Side) -> Pixel {
        match side {
            Side::Primary => self.model.primary(),
            Side::Secondary => self.model.secondary(),
        }
    }

    fn set_edited(&mut self, side: Side, color: Pixel) {
        match side {
            Side::Primary => self.model.set_primary(color),
            Side::Secondary => self.model.set_secondary(color),
        }
    }

    /// Closes the editor, keeping the edit or restoring the colour it began
    /// with, and gives the canvas back.
    pub(super) fn close_editor(&mut self, ui: &Ui<Msg>, keep: bool) {
        if let Some((side, original)) = self.editing.take()
            && !keep
        {
            match side {
                Side::Primary => self.model.set_primary(original),
                Side::Secondary => self.model.set_secondary(original),
            }
        }
        self.editor.close(ui);
        self.canvas.set_visible(true);
        ui.focus(self.canvas.id());
    }

    /// Restores the default colours.
    pub(super) fn reset_colors(&mut self) {
        self.model.set_primary(DEFAULT_PRIMARY);
        self.model.set_secondary(DEFAULT_SECONDARY);
    }
}
