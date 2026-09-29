#![forbid(unsafe_code)]

//! The notepad app's state, message type and update dispatcher.
//!
//! `update` is deliberately thin: it maps each [`Msg`] to a `commands::*`
//! function, which owns the behaviour. Widgets map their events to `Msg` through
//! closures fixed at construction, so there are no numeric control ids.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use xui_code_editor::{Document, Editor, SearchState};
use xui_core::app::Ui;
use xui_core::arrange::Mounted;
use xui_core::backend::WidgetId;
use xui_core::message::{Key, Modifiers};
use xui_core::widget::{
    Button, CheckBox, Dialog, DialogAction, Edit, FileDialog, Label, StatusBar,
};

use crate::commands;

/// The app's message: one variant per user intent.
pub enum Msg {
    /// A menu command or shortcut asked for a new document.
    New,
    /// Open a path (after any discard confirmation).
    Open,
    /// Save the current document.
    Save,
    /// Prompt for a path and save there.
    SaveAs,
    /// Quit the app.
    Quit,
    /// The window's close button was pressed.
    CloseRequested,
    /// Menu/command editing commands the editor handles via its own keys too,
    /// mapped here for the Cmd key on macOS.
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    /// Show the find bar.
    Find,
    /// Show the find bar for replace.
    Replace,
    /// Select the next match.
    FindNext,
    /// Select the previous match.
    FindPrevious,
    /// Replace the current match, or advance when it is not one.
    ReplaceCurrent,
    /// Replace every match as one undo step.
    ReplaceAll,
    /// The query field changed.
    QueryChanged(String),
    /// The Regex check box changed.
    RegexToggled(bool),
    /// The Match case check box changed.
    CaseToggled(bool),
    /// Hide the find bar.
    CloseFind,
    /// A dialog was dismissed.
    Dialog(DialogAction),
    /// The Open picker returned a path.
    OpenChosen(PathBuf),
    /// The Save As picker returned a path.
    SaveChosen(PathBuf),
    /// The editor's text changed.
    Edited,
    /// Refresh the status bar (a caret move with no text change).
    RefreshStatus,
}

/// What to do once a discard confirmation is accepted.
pub enum After {
    /// Start a new document.
    New,
    /// Open the path prompt.
    Open,
    /// Quit.
    Quit,
}

/// What the app is waiting for from a dialog.
pub enum Pending {
    /// Nothing.
    None,
    /// Confirmation to discard unsaved changes before `After`.
    DiscardThen(After),
}

/// The find/replace bar's widgets. They are hidden together and shown after
/// Find/Replace.
pub struct FindBar {
    /// The query field.
    pub query: Rc<Edit<Msg>>,
    /// The replacement field.
    pub replacement: Rc<Edit<Msg>>,
    /// The "3 of 17" or error label.
    pub status: Rc<Label<Msg>>,
    /// The Regex check box.
    pub regex: Rc<CheckBox<Msg>>,
    /// The Match case check box.
    pub case: Rc<CheckBox<Msg>>,
    /// Next, Previous, Replace and Replace all buttons.
    pub buttons: [Rc<Button<Msg>>; 4],
    /// Every node in the bar, for one show/hide call.
    pub nodes: Vec<WidgetId>,
}

impl FindBar {
    /// Shows or hides the whole bar; a mounted layout re-flows.
    pub fn set_visible(&self, ui: &Ui<Msg>, visible: bool) {
        for id in &self.nodes {
            ui.set_visible(*id, visible);
        }
    }

    /// The replacement text as typed.
    pub fn replacement_text(&self) -> String {
        use xui_core::widget::HasText;
        self.replacement.text()
    }
}

/// The notepad application.
pub struct Notepad {
    /// The editing widget.
    pub editor: Rc<Editor<Msg>>,
    /// The file the buffer belongs to.
    pub document: Document,
    /// The find/replace session.
    pub search: SearchState,
    /// The find/replace bar.
    pub find_bar: FindBar,
    /// The status bar.
    pub status: Rc<StatusBar<Msg>>,
    /// The portable file picker for opening a file.
    pub open_dialog: FileDialog<Msg>,
    /// The portable file picker for Save As.
    pub save_dialog: FileDialog<Msg>,
    /// The discard confirmation.
    pub confirm: Dialog<Msg>,
    /// The error message dialog.
    pub message: Dialog<Msg>,
    /// What the open dialog is for.
    pub pending: Pending,
    /// Whether any dialog is open, so shortcut keys leave it alone.
    pub dialog_open: Rc<Cell<bool>>,
    /// Whether the find bar is open, so Escape closes it.
    pub find_open: Rc<Cell<bool>>,
    /// The mounted widget tree; dropping it destroys the widgets.
    pub _mounted: Mounted<Msg>,
}

/// Maps a key to a shortcut, if it is one this app owns.
///
/// The editor is focused for most of the app's life, so a shortcut must be a key
/// the editor leaves unhandled: Ctrl/Cmd+N/O/S, Ctrl+Shift+S, Ctrl/Cmd+F/H,
/// F3/Shift+F3 and Escape while the find bar is open. On macOS the command key is
/// [`Modifiers::win`], so the Cmd forms of the editor's own Ctrl editing commands
/// are routed to it here too. Navigation keys raise [`Msg::RefreshStatus`] so the
/// caret readout follows, and are not consumed.
pub fn shortcut(
    key: Key,
    modifiers: Modifiers,
    dialog_open: &Cell<bool>,
    find_open: &Cell<bool>,
) -> Option<Msg> {
    if dialog_open.get() {
        return None;
    }
    if key == Key::F3 {
        return Some(if modifiers.shift {
            Msg::FindPrevious
        } else {
            Msg::FindNext
        });
    }
    if key == Key::ESCAPE && find_open.get() {
        return Some(Msg::CloseFind);
    }
    let command = modifiers.ctrl || modifiers.win;
    if command {
        match key {
            Key::N => return Some(Msg::New),
            Key::O => return Some(Msg::Open),
            Key::S if modifiers.shift => return Some(Msg::SaveAs),
            Key::S => return Some(Msg::Save),
            Key::F => return Some(Msg::Find),
            Key::H => return Some(Msg::Replace),
            _ => {}
        }
    }
    // On macOS Cmd is `win`; the editor only knows Ctrl, so the app routes the
    // Cmd forms. On Windows Ctrl is handled inside the editor and left alone.
    if modifiers.win && !modifiers.ctrl {
        match key {
            Key::C => return Some(Msg::Copy),
            Key::X => return Some(Msg::Cut),
            Key::V => return Some(Msg::Paste),
            Key::Z if modifiers.shift => return Some(Msg::Redo),
            Key::Z => return Some(Msg::Undo),
            Key::Y => return Some(Msg::Redo),
            Key::A => return Some(Msg::SelectAll),
            _ => {}
        }
    }
    if is_caret_key(key) {
        return Some(Msg::RefreshStatus);
    }
    None
}

/// Whether `key` moves the caret (and so changes the Ln/Col readout) without
/// changing the text.
fn is_caret_key(key: Key) -> bool {
    matches!(
        key,
        Key::LEFT
            | Key::RIGHT
            | Key::UP
            | Key::DOWN
            | Key::HOME
            | Key::END
            | Key::PAGE_UP
            | Key::PAGE_DOWN
            | Key::BACK
            | Key::DELETE
            | Key::RETURN
            | Key::TAB
    )
}

impl xui_core::app::App for Notepad {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::New => commands::new_document(self, ui),
            Msg::Open => commands::open(self, ui),
            Msg::Save => commands::save(self, ui),
            Msg::SaveAs => commands::save_as_prompt(self, ui),
            Msg::Quit | Msg::CloseRequested => commands::quit(self, ui),
            Msg::Undo => commands::undo(self, ui),
            Msg::Redo => commands::redo(self, ui),
            Msg::Cut => commands::cut(self, ui),
            Msg::Copy => commands::copy(self, ui),
            Msg::Paste => commands::paste(self, ui),
            Msg::SelectAll => commands::select_all(self, ui),
            Msg::Find => commands::show_find(self, ui),
            Msg::Replace => commands::show_find(self, ui),
            Msg::FindNext => commands::find_step(self, ui, true),
            Msg::FindPrevious => commands::find_step(self, ui, false),
            Msg::ReplaceCurrent => commands::replace_current(self, ui),
            Msg::ReplaceAll => commands::replace_all(self, ui),
            Msg::QueryChanged(text) => {
                self.search.query_text = text;
                commands::find_refresh(self, ui);
            }
            Msg::RegexToggled(regex) => {
                self.search.regex = regex;
                commands::find_refresh(self, ui);
            }
            Msg::CaseToggled(case_sensitive) => {
                self.search.case_sensitive = case_sensitive;
                commands::find_refresh(self, ui);
            }
            Msg::CloseFind => commands::close_find(self, ui),
            Msg::Dialog(action) => commands::dialog_action(self, ui, action),
            Msg::OpenChosen(path) => commands::open_chosen(self, ui, path),
            Msg::SaveChosen(path) => commands::save_chosen(self, ui, path),
            Msg::Edited => commands::edited(self, ui),
            Msg::RefreshStatus => commands::refresh(self, ui),
        }
    }
}
