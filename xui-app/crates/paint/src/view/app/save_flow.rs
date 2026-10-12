#![forbid(unsafe_code)]

//! Save and Save As: which format a name means, the "Flatten image?" question
//! and the write.
//!
//! The format comes from the file name's extension (`.ora`, `.png`, `.bmp`);
//! a name with none gets `.ora` when the picture has layers and `.png`
//! otherwise. JPEG opens but is not written yet, and says so. A picture with
//! layers saved to a format that has none is flattened *into the file only*:
//! the picture in Paint keeps its layers.

use std::path::{Path, PathBuf};

use xui_core::app::Ui;
use xui_core::widget::TaskDialogAction;

use super::files::display_name;
use super::{Msg, PaintApp};
use crate::model::Format;
use crate::view::Report;

/// The message for a format Paint cannot write.
fn cannot_save(format: Format) -> String {
    format!(
        "{} files cannot be saved yet; save as .ora, .png or .bmp",
        format.name()
    )
}

impl PaintApp {
    /// The extension a name without a usable one gets.
    fn default_format(&self) -> Format {
        if self.model.document().layers().len() > 1 {
            Format::OpenRaster
        } else {
            Format::Png
        }
    }

    /// The name the Save As picker suggests: the current name when its format
    /// can hold the picture, else its stem with the default extension.
    fn suggested_name(&self) -> String {
        let default = self.default_format();
        let Some(path) = self.current_path.as_deref() else {
            return format!("untitled.{}", default.extension());
        };
        let keeps =
            Format::from_path(path).is_some_and(|f| f.can_save() && !self.model.needs_flatten(f));
        if keeps {
            return display_name(path);
        }
        let stem = path
            .file_stem()
            .map_or_else(|| "untitled".into(), |s| s.to_string_lossy().into_owned());
        format!("{stem}.{}", default.extension())
    }

    /// `path` with an extension Paint can write, or why it cannot be.
    fn with_extension(&self, path: PathBuf) -> Result<PathBuf, String> {
        match Format::from_path(&path) {
            Some(format) if format.can_save() => Ok(path),
            Some(format) => Err(cannot_save(format)),
            None => {
                let mut name = path.into_os_string();
                name.push(format!(".{}", self.default_format().extension()));
                Ok(PathBuf::from(name))
            }
        }
    }

    /// Save: to the current path when it names a format Paint writes, else
    /// through Save As. A store with no paths just saves.
    pub(super) fn save_requested(&mut self, ui: &Ui<Msg>) {
        if self.files.is_none() {
            if self.save() {
                self.finish_after(ui);
            } else {
                self.after = None;
            }
            return;
        }
        match self.current_path.clone() {
            Some(path) if Format::from_path(&path).is_some_and(Format::can_save) => {
                self.save_to(ui, &path);
            }
            Some(path) => {
                // Opened from a JPEG (or a name with no format): say why the
                // picker comes up instead of silently writing PNG over it.
                if let Some(format) = Format::from_path(&path) {
                    self.message = Some(cannot_save(format));
                }
                self.save_as_requested();
            }
            None => self.save_as_requested(),
        }
    }

    /// Save As: always the picker (a plain Save when none is wired).
    pub(super) fn save_as_requested(&mut self) {
        let Some(files) = &self.files else {
            self.save();
            return;
        };
        let name = self.suggested_name();
        files.show_save(&name, self.current_path.as_deref().and_then(Path::parent));
    }

    /// The Save As picker returned a path.
    pub(super) fn save_chosen(&mut self, ui: &Ui<Msg>) {
        ui.focus(self.canvas.id());
        let Some(chosen) = self.files.as_ref().and_then(|files| files.take_chosen()) else {
            return;
        };
        let path = match self.with_extension(chosen.clone()) {
            Ok(path) => path,
            Err(error) => {
                self.message = Some(error);
                self.after = None;
                return;
            }
        };
        // The picker confirmed overwriting `chosen`; a path it did not see must
        // not be replaced silently.
        let unseen = path != chosen && self.files.as_ref().is_some_and(|f| f.exists(&path));
        if unseen {
            self.message = Some(format!("Not saved: {} already exists", display_name(&path)));
            self.after = None;
            return;
        }
        self.save_to(ui, &path);
    }

    /// Writes to `path`, first asking "Flatten image?" when the picture has
    /// layers the format cannot keep.
    fn save_to(&mut self, ui: &Ui<Msg>, path: &Path) {
        let Some(format) = Format::from_path(path).filter(|f| f.can_save()) else {
            self.message = Some(match Format::from_path(path) {
                Some(format) => cannot_save(format),
                None => "Not saved: the name has no format".to_string(),
            });
            self.after = None;
            return;
        };
        if self.model.needs_flatten(format) {
            let layers = self.model.document().layers().len();
            match self.flatten_prompt.show(ui, format.name(), layers) {
                Ok(()) => self.pending_save = Some((path.to_path_buf(), format)),
                Err(error) => {
                    self.message = Some(format!("Save failed: {error}"));
                    self.after = None;
                }
            }
            return;
        }
        self.write_file(ui, path, format);
    }

    /// The "Flatten image?" card was answered.
    pub(super) fn flatten_answer(&mut self, ui: &Ui<Msg>, action: TaskDialogAction) {
        self.flatten_prompt.close();
        ui.focus(self.canvas.id());
        let Some((path, format)) = self.pending_save.take() else {
            return;
        };
        if action == TaskDialogAction::Command(0) {
            self.write_file(ui, &path, format);
        } else {
            self.after = None;
        }
    }

    /// Encodes the picture as `format` and writes it to `path`. The document
    /// is untouched (a flat format gets a flattened copy); on failure the path
    /// and the unsaved state stay as they were.
    fn write_file(&mut self, ui: &Ui<Msg>, path: &Path, format: Format) {
        let name = display_name(path);
        let result = self
            .model
            .save_bytes(format)
            .and_then(|bytes| self.storage.save_to(path, &bytes));
        match result {
            Ok(()) => {
                self.current_path = Some(path.to_path_buf());
                self.mark_clean();
                self.message = Some(format!("Saved {name}"));
                self.announce(Report::Saved {
                    path: Some(path.to_path_buf()),
                    format,
                });
                self.finish_after(ui);
            }
            Err(error) => {
                self.message = Some(format!("Save failed: {name}: {error}"));
                self.after = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_is_refused_with_the_formats_that_work() {
        let message = cannot_save(Format::Jpeg);
        assert!(message.contains("JPEG"));
        for ok in [".ora", ".png", ".bmp"] {
            assert!(message.contains(ok), "{message}");
        }
    }
}
