//! The right-pane editor state machine, independent of any widget.
//!
//! Two editors live here: [`KeyEditor`] edits the selected existing key (with
//! lazy load, dirty tracking, a two-step delete and external-change detection)
//! and [`NewKeyEditor`] creates a key (validated, with a two-step overwrite
//! confirm). Both are plain data with `&dyn ConfStore` methods, so the whole
//! behaviour is host-testable without a window.

use confd::Value;

use crate::store::{ConfStore, StoreError};
use crate::tree::{Row, Tree};
use crate::value_edit::{self, Kind};

/// Reloads `tree` from the store and returns its new visible rows.
///
/// On a `List` failure the previous tree is left exactly as it was, so a
/// transient error does not wipe the view.
pub fn reload_tree(
    tree: &mut Tree,
    filter: &str,
    store: &dyn ConfStore,
) -> Result<Vec<Row>, StoreError> {
    let paths = store.list("")?;
    tree.refresh(paths);
    Ok(tree.rows(filter))
}

/// The result of a create attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreateOutcome {
    /// The key was written.
    Created,
    /// A value already exists; a second explicit click overwrites it.
    NeedsConfirm,
    /// The request was rejected, with the reason.
    Failed(String),
}

/// The editor for the selected key.
#[derive(Debug)]
pub struct KeyEditor {
    path: Option<String>,
    kind: Kind,
    text: String,
    stored: Option<Value>,
    /// The delete button's second-click confirmation.
    pub confirm_delete: bool,
    /// Set when the store changed under an unsaved buffer.
    pub external_changed: bool,
    /// Set when a read or write was denied: every later write is refused.
    pub read_only: bool,
    note: Option<String>,
}

impl Default for KeyEditor {
    fn default() -> KeyEditor {
        KeyEditor {
            path: None,
            kind: Kind::Bool,
            text: String::new(),
            stored: None,
            confirm_delete: false,
            external_changed: false,
            read_only: false,
            note: None,
        }
    }
}

impl KeyEditor {
    /// The selected path, if any.
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// The kind the buffer is being edited as.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The current edit buffer.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The value last read or written, if the key exists.
    pub fn stored(&self) -> Option<&Value> {
        self.stored.as_ref()
    }

    /// A note about the key's state (deleted, read-only, changed externally).
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Whether the buffer differs from the stored value.
    pub fn dirty(&self) -> bool {
        match &self.stored {
            Some(value) => {
                self.text != value_edit::format(value) || self.kind != Kind::from_value(value)
            }
            None => !self.text.is_empty(),
        }
    }

    /// Selects `path` (or clears the pane) and loads its value lazily.
    ///
    /// Re-selecting the same path is a no-op so unsaved edits survive a
    /// re-render; selecting a different path resets the buffer to that path's
    /// value, so a buffer is never applied to the wrong key.
    pub fn select(&mut self, path: Option<String>, store: &dyn ConfStore) {
        if self.path == path && self.path.is_some() {
            return;
        }
        self.reset();
        self.path = path;
        if self.path.is_some() {
            self.load(store);
        }
    }

    /// Replaces the edit buffer with the stored value (discarding edits).
    pub fn revert(&mut self) {
        self.confirm_delete = false;
        self.external_changed = false;
        match self.stored.clone() {
            Some(value) => {
                self.kind = Kind::from_value(&value);
                self.text = value_edit::format(&value);
                self.note = None;
            }
            None => self.text.clear(),
        }
    }

    /// Replaces the edit buffer text (from the value field).
    pub fn set_text(&mut self, text: String) {
        self.text = text;
    }

    /// Flips a bool buffer between `true` and `false`.
    pub fn toggle_bool(&mut self) {
        self.text = if self.text.trim().eq_ignore_ascii_case("true") {
            "false".into()
        } else {
            "true".into()
        };
    }

    /// Changes the kind, re-parsing the buffer first.
    ///
    /// On a parse failure the old kind and value are kept and the reason is
    /// returned, so an existing key can never be relabelled to a kind its text
    /// cannot represent.
    pub fn set_kind(&mut self, kind: Kind) -> Result<(), String> {
        if kind == self.kind {
            return Ok(());
        }
        match value_edit::parse(kind, &self.text) {
            Ok(_) => {
                self.kind = kind;
                self.note = None;
                Ok(())
            }
            Err(error) => {
                self.note = Some(error.clone());
                Err(error)
            }
        }
    }

    /// Arms the delete confirmation; `true` means the click should delete.
    pub fn arm_delete(&mut self) -> bool {
        if self.confirm_delete {
            true
        } else {
            self.confirm_delete = true;
            false
        }
    }

    /// Clears a pending delete confirmation (any other action cancels it).
    pub fn clear_confirm(&mut self) {
        self.confirm_delete = false;
    }

    /// Parses the buffer and writes it; on any failure nothing changes.
    pub fn apply(&mut self, store: &dyn ConfStore) -> Result<String, String> {
        let path = self
            .path
            .clone()
            .ok_or_else(|| "select a key first".to_string())?;
        if self.read_only {
            return Err("this key is read-only".into());
        }
        let value = value_edit::parse(self.kind, &self.text)?;
        match store.set(&path, value.clone()) {
            Ok(()) => {
                self.stored = Some(value);
                self.confirm_delete = false;
                self.external_changed = false;
                self.note = None;
                Ok(format!("saved {path}"))
            }
            Err(StoreError::Denied) => {
                self.read_only = true;
                Err(StoreError::Denied.message())
            }
            Err(error) => Err(error.message()),
        }
    }

    /// Deletes the selected key; on failure nothing changes.
    pub fn delete(&mut self, store: &dyn ConfStore) -> Result<String, String> {
        let path = self
            .path
            .clone()
            .ok_or_else(|| "select a key first".to_string())?;
        if self.read_only {
            return Err("this key is read-only".into());
        }
        match store.delete(&path) {
            Ok(()) => {
                self.stored = None;
                self.text.clear();
                self.confirm_delete = false;
                self.external_changed = false;
                self.note = Some("deleted; Refresh updates the tree".into());
                Ok(format!("deleted {path}"))
            }
            Err(StoreError::Denied) => {
                self.read_only = true;
                Err(StoreError::Denied.message())
            }
            Err(error) => Err(error.message()),
        }
    }

    /// Re-reads the selected key after a `Refresh`.
    ///
    /// A change under an unsaved buffer is not applied to the buffer; the
    /// editor flags it so the user can [`KeyEditor::reload`] deliberately.
    pub fn refresh(&mut self, store: &dyn ConfStore) {
        let Some(path) = self.path.clone() else {
            return;
        };
        match store.get(&path) {
            Ok(Some(value)) => {
                if self.dirty() {
                    // Never overwrite an unsaved buffer; only flag a real
                    // external change so the user can Reload deliberately.
                    if self.stored.as_ref() != Some(&value) {
                        self.external_changed = true;
                        self.note = Some("changed externally; Reload to discard edits".into());
                    }
                } else {
                    self.kind = Kind::from_value(&value);
                    self.text = value_edit::format(&value);
                    self.stored = Some(value);
                    self.external_changed = false;
                    self.note = None;
                }
            }
            Ok(None) => {
                if self.dirty() {
                    self.external_changed = true;
                    self.note = Some("deleted externally; Reload to discard edits".into());
                } else {
                    self.stored = None;
                    self.text.clear();
                    self.external_changed = false;
                    self.note = Some("that key no longer exists".into());
                }
            }
            Err(StoreError::Denied) => {
                self.read_only = true;
                self.note = Some("you may not read this key (read-only)".into());
            }
            Err(error) => self.note = Some(error.message()),
        }
    }

    /// Discards any external-change warning and reloads from the store.
    pub fn reload(&mut self, store: &dyn ConfStore) {
        self.external_changed = false;
        self.load(store);
    }

    /// Resets the buffer and state.
    fn reset(&mut self) {
        self.kind = Kind::Bool;
        self.text.clear();
        self.stored = None;
        self.confirm_delete = false;
        self.external_changed = false;
        self.read_only = false;
        self.note = None;
    }

    /// Reads the selected path and points the buffer at it.
    fn load(&mut self, store: &dyn ConfStore) {
        let Some(path) = self.path.clone() else {
            return;
        };
        match store.get(&path) {
            Ok(Some(value)) => {
                self.kind = Kind::from_value(&value);
                self.text = value_edit::format(&value);
                self.stored = Some(value);
                self.read_only = false;
                self.note = None;
            }
            Ok(None) => {
                self.stored = None;
                self.text.clear();
                self.note = Some("that key no longer exists".into());
            }
            Err(StoreError::Denied) => {
                self.read_only = true;
                self.stored = None;
                self.text.clear();
                self.note = Some("you may not read this key (read-only)".into());
            }
            Err(error) => {
                self.stored = None;
                self.text.clear();
                self.note = Some(error.message());
            }
        }
    }
}

/// The editor for a key being created.
#[derive(Debug)]
pub struct NewKeyEditor {
    /// Whether the create pane is shown.
    pub active: bool,
    /// The path being typed.
    pub path: String,
    /// The kind the value is parsed as.
    pub kind: Kind,
    /// The value text.
    pub text: String,
    /// The overwrite confirmation's second-click flag.
    pub confirm_clobber: bool,
}

impl Default for NewKeyEditor {
    fn default() -> NewKeyEditor {
        NewKeyEditor {
            active: false,
            path: String::new(),
            kind: Kind::Str,
            text: String::new(),
            confirm_clobber: false,
        }
    }
}

impl NewKeyEditor {
    /// Clears a pending overwrite confirmation (any other action cancels it).
    pub fn clear_confirm(&mut self) {
        self.confirm_clobber = false;
    }

    /// Validates and creates the key, refusing to clobber without a confirm.
    pub fn create(&mut self, store: &dyn ConfStore) -> CreateOutcome {
        let path = self.path.trim().to_owned();
        if confd::validate_path(&path).is_err() {
            return CreateOutcome::Failed("that is not a valid confd path".into());
        }
        match store.get(&path) {
            Ok(Some(_)) if !self.confirm_clobber => {
                self.confirm_clobber = true;
                return CreateOutcome::NeedsConfirm;
            }
            Err(error) => return CreateOutcome::Failed(error.message()),
            _ => {}
        }
        let value = match value_edit::parse(self.kind, &self.text) {
            Ok(value) => value,
            Err(error) => return CreateOutcome::Failed(error),
        };
        match store.set(&path, value) {
            Ok(()) => {
                self.path.clear();
                self.text.clear();
                self.confirm_clobber = false;
                CreateOutcome::Created
            }
            Err(error) => CreateOutcome::Failed(error.message()),
        }
    }
}

#[cfg(test)]
mod tests;
