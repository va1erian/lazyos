//! The Config window: a tree of every `confd` key on the left, the selected
//! key's kind and value on the right.
//!
//! The heavy lifting lives in [`crate::sections`] (the editor state machine),
//! [`crate::tree`] (the tree projection) and [`crate::value_edit`] (the value
//! grammar); this module only lays the widgets out and mirrors the state back
//! onto them. A `Refresh` re-lists the tree and re-reads the selected key
//! (never clobbering a dirty buffer); live change-topic subscription is not
//! wired, see `docs/confd-editor.md`.

use std::rc::Rc;

use xui_core::app::{App, Ui};
use xui_core::arrange::{column, label, row, Handle, LayoutExt, Mounted};
use xui_core::backend::Result;
use xui_core::layout::Insets;
use xui_core::{Dip, HasText};

use crate::sections::{self, CreateOutcome, KeyEditor, NewKeyEditor};
use crate::store::ConfStore;
use crate::tree::{Row, Tree};
use crate::value_edit::{self, Kind};
use crate::view::{card, Widgets};

/// Window size (DIP) the app asks for.
pub const WINDOW: (i32, i32) = (720, 500);
/// Width of the tree pane.
const LEFT_W: i32 = 300;
/// Height reserved at the bottom for the status line.
const STATUS_H: i32 = 26;

/// Messages the widgets raise.
#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    /// A tree row was selected (folders toggle, leaves load).
    Select(usize),
    /// The path filter changed.
    Filter(String),
    /// The selected key's kind changed.
    Kind(usize),
    /// The value buffer changed.
    Value(String),
    /// The bool value's toggle button was clicked.
    ToggleBool,
    /// Write the buffer.
    Apply,
    /// Discard edits.
    Revert,
    /// Delete the selected key (two clicks).
    Delete,
    /// Take the external value, discarding edits.
    Reload,
    /// Re-list the tree and re-read the selected key.
    Refresh,
    /// Show or hide the create-key pane.
    NewToggle,
    /// The new key's path changed.
    NewPath(String),
    /// The new key's kind changed.
    NewKind(usize),
    /// The new key's value changed.
    NewValue(String),
    /// Create the new key (two clicks when it exists).
    Create,
    /// The compositor asked the window to close.
    Close,
}

/// The Config app.
pub struct ConfdEditorApp {
    store: Rc<dyn ConfStore>,
    tree: Tree,
    visible: Vec<Row>,
    /// The labels last pushed to the `ListView`, so an unrelated update does
    /// not reset its scroll position.
    rendered: Vec<String>,
    filter: String,
    editor: KeyEditor,
    new_key: NewKeyEditor,
    persistent: bool,
    status_text: String,
    banner_text: String,
    widgets: Widgets,
    _panes: [Mounted<Msg>; 2],
}

/// Sets a widget's text only when it differs, so a focused field's caret is
/// not reset on every render.
fn set_text(widget: &impl HasText, text: &str) {
    if widget.text() != text {
        widget.set_text(text);
    }
}

impl ConfdEditorApp {
    /// Builds the window's widgets over `store`.
    pub fn build(ui: &mut Ui<Msg>, store: Rc<dyn ConfStore>) -> Result<ConfdEditorApp> {
        ui.on_close(|| Some(Msg::Close));
        let widgets = Widgets::default();
        let (left, right) = (Handle::new(), Handle::new());
        ui.root(
            column().children((
                row()
                    .children((
                        card().bind(&left).width(LEFT_W),
                        card().bind(&right).fill(1),
                    ))
                    .fill(1),
                row()
                    .padding(Insets::symmetric(Dip(10.0), Dip(3.0)))
                    .child(label("").bind(&widgets.status).fill(1))
                    .fixed(STATUS_H),
            )),
        )?;
        let panes = [
            ui.mount_in(left.get().widget.id(), widgets.tree_pane())?,
            ui.mount_in(right.get().widget.id(), widgets.key_pane())?,
        ];

        let mut app = ConfdEditorApp {
            store,
            tree: Tree::default(),
            visible: Vec::new(),
            rendered: Vec::new(),
            filter: String::new(),
            editor: KeyEditor::default(),
            new_key: NewKeyEditor::default(),
            persistent: true,
            status_text: String::new(),
            banner_text: String::new(),
            widgets,
            _panes: panes,
        };
        match app.store.info() {
            Ok(info) => app.persistent = info.persistent,
            Err(error) => app.status_text = error.message(),
        }
        app.banner_text = if app.persistent {
            String::new()
        } else {
            "Store is not persistent: values will not survive a reboot.".into()
        };
        app.reload_tree();
        app.sync(ui);
        Ok(app)
    }

    /// Re-lists the tree, keeping the previous one on a list error, and
    /// re-reads the selected key without clobbering a dirty buffer.
    fn reload_tree(&mut self) {
        match sections::reload_tree(&mut self.tree, &self.filter, self.store.as_ref()) {
            Ok(rows) => {
                self.visible = rows;
                self.editor.refresh(self.store.as_ref());
            }
            Err(error) => self.set_status(error.message()),
        }
    }

    /// Records a status message (shown on the next render).
    fn set_status(&mut self, text: String) {
        self.status_text = text;
    }

    /// The preview/state line under the value field.
    fn preview_text(&self) -> String {
        if self.editor.read_only {
            return "read-only: you may not write this key".into();
        }
        if self.editor.external_changed {
            return "changed externally \u{2014} Reload to discard your edits".into();
        }
        if let Some(note) = self.editor.note() {
            return note.to_owned();
        }
        if self.editor.dirty() {
            return "modified (not applied)".into();
        }
        match self.editor.stored() {
            Some(value) => format!("value: {}", value_edit::preview(value)),
            None => String::new(),
        }
    }

    /// A tree row was clicked: load a leaf, toggle a folder.
    fn select_row(&mut self, index: usize) {
        let Some(row) = self.visible.get(index).cloned() else {
            return;
        };
        if row.leaf {
            self.editor
                .select(Some(row.path.clone()), self.store.as_ref());
        }
        if row.folder {
            self.tree.toggle(&row.path);
            self.visible = self.tree.rows(&self.filter);
        }
    }

    /// The delete button: first click arms, second deletes.
    fn on_delete(&mut self) {
        if !self.editor.arm_delete() {
            self.set_status("Click Delete again to confirm.".into());
            return;
        }
        match self.editor.delete(self.store.as_ref()) {
            Ok(message) => {
                self.set_status(message);
                self.reload_tree();
            }
            Err(error) => self.set_status(error),
        }
    }

    /// The create button, with the overwrite confirmation.
    fn on_create(&mut self) {
        match self.new_key.create(self.store.as_ref()) {
            CreateOutcome::Created => {
                self.set_status("Key created.".into());
                self.reload_tree();
            }
            CreateOutcome::NeedsConfirm => self.set_status(
                "A value already exists at that path; click Create again to overwrite.".into(),
            ),
            CreateOutcome::Failed(error) => self.set_status(error),
        }
    }

    /// Mirrors the model onto every widget.
    fn sync(&mut self, ui: &mut Ui<Msg>) {
        let w = &self.widgets;
        let list = w.list.get();
        let labels: Vec<String> = self.visible.iter().map(Row::label).collect();
        if labels != self.rendered {
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            list.set_items(&refs);
            self.rendered = labels;
        }
        let selected = self
            .editor
            .path()
            .and_then(|path| self.visible.iter().position(|row| row.path == path));
        list.select(selected);

        set_text(&*w.filter.get(), &self.filter);
        w.path.get().set_text(
            &self
                .editor
                .path()
                .map(|path| format!("Key: {path}"))
                .unwrap_or_else(|| "No key selected".into()),
        );
        w.kind.get().widget.select(self.editor.kind().index());
        let (value, bool_button) = (w.value.get(), w.bool_button.get());
        set_text(&*value, self.editor.text());
        bool_button.set_text(self.editor.text());
        let is_bool = self.editor.kind() == Kind::Bool;
        ui.set_visible(value.id(), !is_bool);
        ui.set_visible(bool_button.id(), is_bool);
        let delete = w.delete.get();
        delete.set_text(if self.editor.confirm_delete {
            "Confirm delete"
        } else {
            "Delete"
        });
        // A denied key stays visible but its write controls are disabled; the
        // editor also refuses the write itself, so this is belt and braces.
        let writable = !self.editor.read_only;
        ui.set_enabled(w.apply.get().id(), writable);
        ui.set_enabled(w.revert.get().id(), writable);
        ui.set_enabled(delete.id(), writable);
        w.preview.get().set_text(&self.preview_text());

        w.new_toggle.get().set_text(if self.new_key.active {
            "Cancel new key"
        } else {
            "New key"
        });
        let new_kind = w.new_kind.get();
        new_kind.widget.select(self.new_key.kind.index());
        let (new_path, new_value, create) = (w.new_path.get(), w.new_value.get(), w.create.get());
        set_text(&*new_path, &self.new_key.path);
        set_text(&*new_value, &self.new_key.text);
        create.set_text(if self.new_key.confirm_clobber {
            "Confirm create"
        } else {
            "Create"
        });
        let show_new = self.new_key.active;
        let mut new_ids = vec![new_path.id(), new_value.id(), create.id()];
        new_ids.extend(new_kind.widget.ids());
        for id in new_ids {
            ui.set_visible(id, show_new);
        }

        w.banner.get().set_text(&self.banner_text);
        w.status.get().set_text(&self.status_text);
        // The buttons' labels change their natural widths.
        ui.relayout();
    }
}

impl App for ConfdEditorApp {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        // Serial evidence that input reached the app (see `xui_confd.json`).
        println!("CONFDED:MSG:{msg:?}");
        // Any action other than the two-step confirms cancels a pending one.
        if !matches!(msg, Msg::Delete) {
            self.editor.clear_confirm();
        }
        if !matches!(msg, Msg::Create) {
            self.new_key.clear_confirm();
        }
        match msg {
            Msg::Close => {
                println!("CONFDED:CLOSE:PASS");
                ui.quit();
            }
            Msg::Select(index) => self.select_row(index),
            Msg::Filter(text) => {
                self.filter = text;
                self.visible = self.tree.rows(&self.filter);
            }
            Msg::Kind(index) => {
                if let Err(error) = self.editor.set_kind(Kind::from_index(index)) {
                    self.set_status(error);
                }
            }
            Msg::Value(text) => self.editor.set_text(text),
            Msg::ToggleBool => self.editor.toggle_bool(),
            Msg::Apply => match self.editor.apply(self.store.as_ref()) {
                Ok(message) => {
                    self.set_status(message);
                    self.reload_tree();
                }
                Err(error) => self.set_status(error),
            },
            Msg::Revert => self.editor.revert(),
            Msg::Delete => self.on_delete(),
            Msg::Reload => self.editor.reload(self.store.as_ref()),
            Msg::Refresh => self.reload_tree(),
            Msg::NewToggle => self.new_key.active = !self.new_key.active,
            Msg::NewPath(text) => self.new_key.path = text,
            Msg::NewKind(index) => self.new_key.kind = Kind::from_index(index),
            Msg::NewValue(text) => self.new_key.text = text,
            Msg::Create => self.on_create(),
        }
        self.sync(ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_is_wide_enough_for_both_panes() {
        assert!(LEFT_W < WINDOW.0);
        assert!(STATUS_H > 0 && STATUS_H < WINDOW.1);
    }

    #[test]
    fn every_kind_has_a_radio_label() {
        assert_eq!(Kind::ALL.len(), 5);
        for kind in Kind::ALL {
            assert!(!kind.label().is_empty());
            assert!(kind.index() < Kind::ALL.len());
        }
    }
}
