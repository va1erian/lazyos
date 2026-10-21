//! Saved scripts: New, Open, Save and Save As
//! (`docs/messenger-playground-plan.md` P5, saved scripts under
//! `$HOME/.apps/os.lazy.playground/`).
//!
//! A file's text arrives as [`Provenance::File`]: opening it only fills the
//! editor, and the first Run of it asks.

use std::path::{Path, PathBuf};

use xui_app::devtool::Provenance;
use xui_app::platform::dirs;
use xui_core::widget::HasText;

use crate::app::Playground;

/// The package's system name, the folder under `$HOME/.apps`.
const SYSTEM_NAME: &str = "os.lazy.playground";
/// The most a script file may hold.
const MAX_SCRIPT_BYTES: u64 = 1 << 20;

/// The folder scripts are saved in.
pub fn scripts_dir() -> PathBuf {
    PathBuf::from(fhs::app_data_dir(
        &dirs::default_dir().to_string_lossy(),
        SYSTEM_NAME,
    ))
}

impl Playground {
    pub fn update_file_label(&self) {
        let name = match (&self.file, &self.file_label_override) {
            (Some(path), _) => path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            (None, Some(label)) => label.clone(),
            (None, None) => "script.rhai (unsaved)".to_string(),
        };
        let mark = if self.dirty { " *" } else { "" };
        self.w.file.get().set_text(&format!("{name}{mark}"));
    }

    pub fn file_new(&mut self) {
        self.file = None;
        self.file_label_override = None;
        self.set_buffer("", None);
        self.dirty = false;
        self.update_file_label();
        self.focus_editor();
    }

    pub fn file_open(&mut self) {
        self.open_dialog.open();
    }

    pub fn open_chosen(&mut self, path: PathBuf) {
        match read_script(&path) {
            Ok(text) => {
                self.file = Some(path.clone());
                self.file_label_override = None;
                self.set_buffer(&text, Some(Provenance::File));
                self.dirty = false;
                self.update_file_label();
                self.editor.goto(0, 0);
                self.note(&format!(
                    "Opened {}. It runs only when you press Run.",
                    path.display()
                ));
            }
            Err(e) => self.note(&format!("cannot open {}: {e}", path.display())),
        }
        self.focus_editor();
    }

    pub fn file_save(&mut self) {
        match self.file.clone() {
            Some(path) => self.save_chosen(path),
            None => self.file_save_as(),
        }
    }

    pub fn file_save_as(&mut self) {
        let _ = std::fs::create_dir_all(scripts_dir());
        self.save_dialog.open();
    }

    pub fn save_chosen(&mut self, path: PathBuf) {
        let path = with_extension(path);
        match std::fs::write(&path, &self.text) {
            Ok(()) => {
                self.file = Some(path.clone());
                self.file_label_override = None;
                self.dirty = false;
                self.update_file_label();
                self.note(&format!("Saved {}", path.display()));
            }
            Err(e) => self.note(&format!("cannot save {}: {e}", path.display())),
        }
        self.focus_editor();
    }
}

fn with_extension(path: PathBuf) -> PathBuf {
    if path.extension().is_none() {
        path.with_extension("rhai")
    } else {
        path
    }
}

fn read_script(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_SCRIPT_BYTES {
        return Err(format!("{} bytes is too large for a script", meta.len()));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    String::from_utf8(bytes).map_err(|_| "not valid UTF-8".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_name_gets_the_rhai_extension_only_when_it_has_none() {
        assert_eq!(
            with_extension(PathBuf::from("/h/a")),
            PathBuf::from("/h/a.rhai")
        );
        assert_eq!(
            with_extension(PathBuf::from("/h/a.txt")),
            PathBuf::from("/h/a.txt")
        );
    }
}
