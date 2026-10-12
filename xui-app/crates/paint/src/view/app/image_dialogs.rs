#![forbid(unsafe_code)]

//! The app side of the Resize and Canvas Size dialogs: open them over the
//! canvas, keep their fields in step, and apply them on OK.
//!
//! Both are frames that take the canvas's place, so nothing of the picture
//! needs a preview. A refusal (an unusable field, or the memory budget) goes to
//! the status bar and leaves the dialog open, so the user can fix the number
//! instead of retyping everything.

use xui_core::app::Ui;

use super::{Msg, PaintApp};
use crate::view::{CanvasSizeMsg, ResizeMsg};

impl PaintApp {
    /// Image > Resize...
    pub(super) fn open_resize(&mut self, ui: &Ui<Msg>) {
        self.frames.resize.open(ui, self.model.document().size());
        self.canvas.set_visible(false);
    }

    /// Image > Canvas Size...
    pub(super) fn open_canvas_size(&mut self, ui: &Ui<Msg>) {
        let secondary = self.model.secondary();
        self.frames
            .canvas_size
            .open(ui, self.model.document().size(), secondary);
        self.canvas.set_visible(false);
    }

    /// Hands the window back to the canvas.
    fn close_image_dialog(&mut self, ui: &Ui<Msg>) {
        self.frames.resize.close(ui);
        self.frames.canvas_size.close(ui);
        self.canvas.set_visible(true);
        ui.focus(self.canvas.id());
    }

    /// A message from the Resize dialog.
    pub(super) fn resize_msg(&mut self, ui: &Ui<Msg>, msg: ResizeMsg) {
        match msg {
            ResizeMsg::Cancel => self.close_image_dialog(ui),
            ResizeMsg::Ok => match self.frames.resize.read() {
                Ok((width, height, how)) => match self.model.resize_image(width, height, how) {
                    Ok(()) => {
                        self.message = Some(format!("Resized to {width} x {height}"));
                        self.close_image_dialog(ui);
                    }
                    Err(error) => self.message = Some(format!("Resize failed: {error}")),
                },
                Err(error) => self.message = Some(format!("Resize failed: {error}")),
            },
            other => self.frames.resize.edited(other),
        }
    }

    /// A message from the Canvas Size dialog.
    pub(super) fn canvas_size_msg(&mut self, ui: &Ui<Msg>, msg: CanvasSizeMsg) {
        match msg {
            CanvasSizeMsg::Cancel => self.close_image_dialog(ui),
            CanvasSizeMsg::Anchor(anchor) => self.frames.canvas_size.pick(anchor),
            CanvasSizeMsg::Width | CanvasSizeMsg::Height | CanvasSizeMsg::Fill => {
                self.frames.canvas_size.edited();
            }
            CanvasSizeMsg::Ok => match self.frames.canvas_size.read() {
                Ok((width, height, anchor, fill)) => {
                    match self.model.canvas_size(width, height, anchor, fill) {
                        Ok(()) => {
                            self.message = Some(format!("Canvas size {width} x {height}"));
                            self.close_image_dialog(ui);
                        }
                        Err(error) => self.message = Some(format!("Canvas Size failed: {error}")),
                    }
                }
                Err(error) => self.message = Some(format!("Canvas Size failed: {error}")),
            },
        }
    }
}
