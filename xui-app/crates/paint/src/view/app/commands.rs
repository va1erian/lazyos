#![forbid(unsafe_code)]

//! The commands the menus and shortcuts raise: zoom, the selection commands,
//! the clipboard and the paste flow.

use std::rc::Rc;

use xui_core::app::Ui;

use super::dialogs::PasteSize;
use super::{Msg, PaintApp};
use crate::model::{Bitmap, Tool};
use crate::view::Command;
use crate::view::sysclip::SystemClipboard;

/// Which of the three pastes is waiting for the canvas-size answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PasteKind {
    /// A floating selection on the active layer.
    Float,
    /// A floating selection on a new layer.
    NewLayer,
    /// A new image.
    NewImage,
}

impl PaintApp {
    /// Hands a system clipboard that carries `image/png` to the app. Copies
    /// that fit its cap are offered to it, and a paste adopts a picture
    /// another application copied.
    pub fn set_system_clipboard(&mut self, clipboard: Rc<dyn SystemClipboard>) {
        self.system = Some(clipboard);
    }

    /// Runs a command; one whose feature a later phase builds only says so.
    pub(super) fn run_command(&mut self, ui: &Ui<Msg>, command: Command) {
        match command {
            Command::ZoomIn => self.canvas.zoom_in(),
            Command::ZoomOut => self.canvas.zoom_out(),
            Command::ZoomToWindow => self.canvas.zoom_fit(),
            Command::ActualSize => self.canvas.actual_size(),
            Command::PixelGrid => {
                self.grid = !self.grid;
                self.canvas.set_grid(self.grid);
                let state = if self.grid { "on" } else { "off" };
                self.message = Some(format!("Pixel grid {state}"));
            }
            Command::SelectAll => self.model.select_all(),
            Command::Deselect => self.model.deselect(),
            Command::InvertSelection => self.model.invert_selection(),
            Command::EraseSelection => self.model.erase_selection(),
            Command::FillSelection => {
                let color = self.model.primary();
                self.model.fill_selection(color);
            }
            Command::CropToSelection => {
                let result = self.model.crop_to_selection();
                self.report(result, "Cropped to the selection");
            }
            Command::Cut => {
                let result = self.model.cut();
                self.copied(result, "Cut");
            }
            Command::Copy => {
                let result = self.model.copy(false);
                self.copied(result, "Copied");
            }
            Command::CopyMerged => {
                let result = self.model.copy(true);
                self.copied(result, "Copied merged");
            }
            Command::Paste => self.paste(ui, PasteKind::Float),
            Command::PasteNewLayer => self.paste(ui, PasteKind::NewLayer),
            Command::PasteNewImage => self.paste(ui, PasteKind::NewImage),
            other => {
                if !self.layer_command(ui, other) {
                    self.message = Some(format!("{} is not available yet", other.label()));
                }
            }
        }
    }

    /// The status message for the outcome of an action.
    fn report(&mut self, result: Result<(), String>, done: &str) {
        self.message = Some(match result {
            Ok(()) => done.to_string(),
            Err(error) => error,
        });
    }

    /// A copy or cut finished: tell the user and offer the picture to the
    /// system clipboard when one is wired and it fits.
    fn copied(&mut self, result: Result<(), String>, done: &str) {
        if result.is_ok() {
            self.offer_system();
        }
        self.report(result, done);
    }

    fn offer_system(&mut self) {
        let Some(system) = self.system.clone() else {
            return;
        };
        let png = self
            .model
            .clipboard()
            .and_then(|clip| clip.bitmap().encode_png().ok());
        match png {
            Some(png) if png.len() <= system.max_bytes() => {
                system.offer_png(&png);
                self.last_offered = Some(png);
                self.local_newest = false;
            }
            // Too big to share: the in-app copy is the newest.
            _ => {
                self.last_offered = None;
                self.local_newest = true;
            }
        }
    }

    /// Takes a picture another application copied into the in-app slot, when
    /// it is newer than our own last copy.
    fn adopt_system_image(&mut self) {
        let Some(system) = self.system.clone() else {
            return;
        };
        if self.local_newest {
            return;
        }
        let Some(png) = system.request_png() else {
            return;
        };
        if self.last_offered.as_ref() == Some(&png) {
            return;
        }
        if let Ok(bitmap) = Bitmap::decode(&png)
            && self.model.set_clipboard(bitmap, (0, 0)).is_ok()
        {
            self.last_offered = Some(png);
        }
    }

    /// Starts a paste, asking first when the picture is larger than the canvas.
    fn paste(&mut self, ui: &Ui<Msg>, kind: PasteKind) {
        self.adopt_system_image();
        let Some(clip) = self.model.clipboard() else {
            self.message = Some("The clipboard has no picture".to_string());
            return;
        };
        let (width, height) = clip.bitmap().size();
        let (doc_w, doc_h) = self.model.document().size();
        if kind != PasteKind::NewImage && (width > doc_w || height > doc_h) {
            self.pending_paste = Some(kind);
            if let Err(error) = self.paste_size.show(ui, (width, height), (doc_w, doc_h)) {
                self.pending_paste = None;
                self.message = Some(format!("Paste failed: {error}"));
            }
            return;
        }
        self.finish_paste(kind);
    }

    /// The paste dialog answered: grow the canvas to fit first, or not.
    pub(super) fn paste_sized(&mut self, size: PasteSize) {
        let Some(kind) = self.pending_paste.take() else {
            return;
        };
        if size == PasteSize::Expand
            && let Some(clip) = self.model.clipboard()
        {
            let (width, height) = clip.bitmap().size();
            let (doc_w, doc_h) = self.model.document().size();
            if let Err(error) = self.model.resize(width.max(doc_w), height.max(doc_h)) {
                self.message = Some(format!("Resize failed: {error}"));
                return;
            }
        }
        self.finish_paste(kind);
    }

    fn finish_paste(&mut self, kind: PasteKind) {
        let at = self.canvas.view_top_left();
        let result = match kind {
            PasteKind::Float => self.model.paste(at),
            PasteKind::NewLayer => self.model.paste_new_layer(at),
            PasteKind::NewImage => {
                let result = self.model.paste_new_image();
                if result.is_ok() {
                    self.current_path = None;
                    self.cursor = None;
                }
                result
            }
        };
        self.report(result, "Pasted");
    }

    /// The selection and float messages that are not canvas input.
    pub(super) fn selection_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Nudge(dx, dy) => self.model.nudge(dx, dy),
            Msg::FloatCommit => self.model.commit_float(),
            Msg::FloatCancel => self.model.cancel_float(),
            Msg::SelectMode(mode) => self.model.set_sel_mode(mode),
            Msg::WandTolerance(percent) => self.model.set_wand_tolerance(percent),
            Msg::WandContiguous(contiguous) => self.model.set_wand_contiguous(contiguous),
            Msg::PasteExpand => self.paste_sized(PasteSize::Expand),
            Msg::PasteKeep => self.paste_sized(PasteSize::Keep),
            _ => {}
        }
    }

    /// Selects `tool`. (`update` has already ended a stroke in progress, so a
    /// key pressed mid-stroke cannot leave pixels the history never saw.)
    pub(super) fn select_tool(&mut self, tool: Tool) {
        self.model.set_tool(tool);
    }
}
