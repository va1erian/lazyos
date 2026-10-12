#![forbid(unsafe_code)]

//! What Enter, Esc and the window's close button mean while something modal
//! is open: the frames' keys, and cancelling whatever is up before Quit asks.

use xui_core::app::{App, Ui};

use super::{Msg, PaintApp};
use crate::view::CanvasMsg;

/// Whether `msg` gets through while a dialog, card or frame is open: its own
/// results, a focus-loss cancel and the canvas zooming itself.
pub(super) fn passes(msg: &Msg) -> bool {
    matches!(
        msg,
        Msg::OpenChosen
            | Msg::SaveChosen
            | Msg::Resize(_)
            | Msg::CanvasSize(_)
            | Msg::Adjust(..)
            | Msg::Unsaved(_)
            | Msg::Flatten(_)
            | Msg::DialogClosed
            | Msg::OpenStartup
            | Msg::ColorEditing(_)
            | Msg::ColorDone
            | Msg::ColorCancel
            | Msg::ColorAlpha(_)
            | Msg::PropsChanged
            | Msg::PropsOk
            | Msg::PropsCancel
            | Msg::Probe
            | Msg::PasteExpand
            | Msg::PasteKeep
            | Msg::PasteCancel
            | Msg::FrameEnter
            | Msg::FrameEnterOk
            | Msg::Canvas(CanvasMsg::Cancel)
            | Msg::Canvas(CanvasMsg::View)
    )
}

impl PaintApp {
    /// The OK and Cancel messages of the frame that is open, if one is: an
    /// Image or Adjustments dialog, the colour editor or Layer Properties.
    pub(super) fn frame_msgs(&self, ui: &Ui<Msg>) -> Option<(Msg, Msg)> {
        self.frames
            .keys(ui)
            .or_else(|| {
                self.editor
                    .is_open(ui)
                    .then_some((Msg::ColorDone, Msg::ColorCancel))
            })
            .or_else(|| {
                self.props
                    .is_open(ui)
                    .then_some((Msg::PropsOk, Msg::PropsCancel))
            })
    }

    /// Cancels whatever modal is open, as its own Cancel would: a card or
    /// picker is closed with what it was for dropped, a frame is cancelled.
    pub(super) fn dismiss_modal(&mut self, ui: &mut Ui<Msg>) {
        if self.paste_size.is_open() {
            self.paste_size.close();
            self.pending_paste = None;
        }
        self.about.close();
        self.unsaved.close();
        self.flatten_prompt.close();
        self.pending_save = None;
        if let Some(files) = &self.files {
            files.close();
        }
        // A card or picker that is dropped cancels what it was holding back.
        self.after = None;
        if let Some((_, cancel)) = self.frame_msgs(ui) {
            // Sent through `update`, it closes the frame the way its button does.
            self.update(cancel, ui);
        }
        ui.focus(self.canvas.id());
    }
}
