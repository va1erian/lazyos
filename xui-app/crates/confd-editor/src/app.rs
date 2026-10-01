//! The Config window: a tree of every `confd` key on the left, the selected
//! key's kind and value on the right.
//!
//! The heavy lifting lives in [`crate::sections`] (the editor state machine),
//! [`crate::tree`] (the tree projection) and [`crate::value_edit`] (the value
//! grammar); this module only wires those to xui widgets and mirrors the state
//! back onto them. A `Refresh` re-lists the tree and re-reads the selected key
//! (never clobbering a dirty buffer); live change-topic subscription is not
//! wired, see `docs/confd-editor.md`.

use std::rc::Rc;

use xui_core::app::{App, Ui};
use xui_core::backend::Result;
use xui_core::widget::{Button, Edit, Label, ListView, Panel, RadioGroup};
use xui_core::{HasText, Rect};

use crate::sections::{self, CreateOutcome, KeyEditor, NewKeyEditor};
use crate::store::ConfStore;
use crate::tree::{Row, Tree};
use crate::value_edit::{self, Kind};

/// Window size (DIP) the app asks for.
pub const WINDOW: (i32, i32) = (720, 500);
/// Width of the tree pane.
const LEFT_W: i32 = 300;
/// Height reserved at the bottom for the status line.
const STATUS_H: i32 = 26;
/// The body height (the window less the status line).
const BODY_H: i32 = WINDOW.1 - STATUS_H;

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
    _left: Panel<Msg>,
    _right: Panel<Msg>,
    filter_edit: Edit<Msg>,
    // Owned only to keep the node registered; Refresh has no per-render state.
    _refresh: Button<Msg>,
    list: ListView<Msg>,
    path_label: Label<Msg>,
    kind: RadioGroup<Msg>,
    value: Edit<Msg>,
    bool_button: Button<Msg>,
    // Apply and Revert are read only to disable them in a read-only state;
    // Reload is owned only to keep its node registered.
    _apply: Button<Msg>,
    _revert: Button<Msg>,
    delete: Button<Msg>,
    _reload: Button<Msg>,
    preview: Label<Msg>,
    new_toggle: Button<Msg>,
    new_path: Edit<Msg>,
    new_kind: RadioGroup<Msg>,
    new_value: Edit<Msg>,
    create: Button<Msg>,
    banner: Label<Msg>,
    status: Label<Msg>,
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    Rect::new(x, y, x + w, y + h)
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

        let left = Panel::new(ui, rect(0, 0, LEFT_W, BODY_H))?;
        let (filter_edit, refresh) = {
            let p = left.ui();
            let filter = Edit::new(p, rect(10, 10, 196, 26), "")?
                .cue("filter paths")
                .on_change(|text| Some(Msg::Filter(text.to_owned())));
            let refresh =
                Button::new(p, rect(212, 10, 78, 26), "Refresh")?.on_click(|| Some(Msg::Refresh));
            (filter, refresh)
        };
        let list = {
            let p = left.ui();
            ListView::new(p, rect(10, 44, 280, 418), &[])?
                .multi_select(false)
                .on_select(|index| Some(Msg::Select(index)))
        };

        let right = Panel::new(ui, rect(LEFT_W, 0, WINDOW.0 - LEFT_W, BODY_H))?;
        let kind_labels: Vec<&str> = Kind::ALL.iter().map(|kind| kind.label()).collect();
        let p = right.ui();
        let path_label = Label::new(p, rect(14, 10, 392, 20), "No key selected")?;
        let kind = RadioGroup::new(p, rect(14, 36, 180, 140), &kind_labels)?
            .on_select(|index| Some(Msg::Kind(index)));
        let value = Edit::new(p, rect(14, 184, 392, 26), "")?
            .on_change(|text| Some(Msg::Value(text.to_owned())));
        let bool_button =
            Button::new(p, rect(14, 184, 120, 28), "false")?.on_click(|| Some(Msg::ToggleBool));
        let apply = Button::new(p, rect(14, 218, 88, 28), "Apply")?.on_click(|| Some(Msg::Apply));
        let revert =
            Button::new(p, rect(110, 218, 88, 28), "Revert")?.on_click(|| Some(Msg::Revert));
        let delete =
            Button::new(p, rect(206, 218, 110, 28), "Delete")?.on_click(|| Some(Msg::Delete));
        let reload =
            Button::new(p, rect(324, 218, 82, 28), "Reload")?.on_click(|| Some(Msg::Reload));
        let preview = Label::new(p, rect(14, 252, 392, 40), "")?;
        let new_toggle =
            Button::new(p, rect(14, 300, 140, 28), "New key")?.on_click(|| Some(Msg::NewToggle));
        let new_path = Edit::new(p, rect(14, 334, 250, 26), "")?
            .cue("sys/... path")
            .on_change(|text| Some(Msg::NewPath(text.to_owned())));
        let new_kind = RadioGroup::new(p, rect(274, 300, 140, 140), &kind_labels)?
            .on_select(|index| Some(Msg::NewKind(index)));
        let new_value = Edit::new(p, rect(14, 366, 250, 26), "")?
            .cue("value")
            .on_change(|text| Some(Msg::NewValue(text.to_owned())));
        let create =
            Button::new(p, rect(14, 398, 120, 28), "Create")?.on_click(|| Some(Msg::Create));
        let banner = Label::new(p, rect(14, 450, 392, 20), "")?;

        let status = Label::new(ui, rect(10, BODY_H + 3, WINDOW.0 - 20, 20), "")?;

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
            _left: left,
            _right: right,
            filter_edit,
            _refresh: refresh,
            list,
            path_label,
            kind,
            value,
            bool_button,
            _apply: apply,
            _revert: revert,
            delete,
            _reload: reload,
            preview,
            new_toggle,
            new_path,
            new_kind,
            new_value,
            create,
            banner,
            status,
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
        let labels: Vec<String> = self.visible.iter().map(Row::label).collect();
        if labels != self.rendered {
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            self.list.set_items(&refs);
            self.rendered = labels;
        }
        let selected = self
            .editor
            .path()
            .and_then(|path| self.visible.iter().position(|row| row.path == path));
        self.list.select(selected);

        set_text(&self.filter_edit, &self.filter);
        self.path_label.set_text(
            &self
                .editor
                .path()
                .map(|path| format!("Key: {path}"))
                .unwrap_or_else(|| "No key selected".into()),
        );
        self.kind.select(self.editor.kind().index());
        set_text(&self.value, self.editor.text());
        self.bool_button.set_text(self.editor.text());
        let is_bool = self.editor.kind() == Kind::Bool;
        ui.set_visible(self.value.id(), !is_bool);
        ui.set_visible(self.bool_button.id(), is_bool);
        self.delete.set_text(if self.editor.confirm_delete {
            "Confirm delete"
        } else {
            "Delete"
        });
        // A denied key stays visible but its write controls are disabled; the
        // editor also refuses the write itself, so this is belt and braces.
        let writable = !self.editor.read_only;
        ui.set_enabled(self._apply.id(), writable);
        ui.set_enabled(self._revert.id(), writable);
        ui.set_enabled(self.delete.id(), writable);
        self.preview.set_text(&self.preview_text());

        self.new_toggle.set_text(if self.new_key.active {
            "Cancel new key"
        } else {
            "New key"
        });
        self.new_kind.select(self.new_key.kind.index());
        set_text(&self.new_path, &self.new_key.path);
        set_text(&self.new_value, &self.new_key.text);
        self.create.set_text(if self.new_key.confirm_clobber {
            "Confirm create"
        } else {
            "Create"
        });
        let show_new = self.new_key.active;
        ui.set_visible(self.new_path.id(), show_new);
        ui.set_visible(self.new_value.id(), show_new);
        ui.set_visible(self.create.id(), show_new);
        for id in self.new_kind.ids() {
            ui.set_visible(id, show_new);
        }

        self.banner.set_text(&self.banner_text);
        self.status.set_text(&self.status_text);
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
        assert!(BODY_H > 0 && BODY_H < WINDOW.1);
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
