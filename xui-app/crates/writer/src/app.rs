#![forbid(unsafe_code)]

//! LazyWriter's state, messages and the thin `update` dispatcher, ported from
//! the wordpad example's `app.rs`.
//!
//! `update` maps each [`Msg`] to a `commands::*` function, which owns the
//! behaviour. Every edit goes through `RichTextEditor::exec`; the app never
//! edits the document directly.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use xui_core::app::{App, Ui};
use xui_core::arrange::Mounted;
use xui_core::message::{Key, Modifiers};
use xui_core::widget::TaskDialog;
use xui_core::widget::{Dialog, DialogAction, FileDialog, Menu, StatusBar, TaskDialogAction};
use xui_rich_text::RichTextEditor;
use xui_rich_text::edit::Command;
use xui_rich_text::model::{Align, ListKind, StyleSummary};

use crate::commands::{self, files};
use crate::host::Host;
use crate::ui::Tools;

/// A character attribute the B / I / U / S buttons toggle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Bold,
    Italic,
    Underline,
    Strike,
}

/// One variant per user intent.
pub enum Msg {
    New,
    Open,
    OpenChosen(PathBuf),
    Save,
    SaveAs,
    SaveChosen(PathBuf),
    Export,
    ExportChosen(PathBuf),
    InsertImage,
    ImageChosen(PathBuf),
    /// Ask for a link address for the selection.
    Link,
    /// The link prompt was dismissed.
    LinkChosen(DialogAction),
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    /// The document changed; it now has this many words.
    Edited(usize),
    /// The selection or the formatting under it changed.
    Selection(StyleSummary),
    /// A link was Ctrl+clicked.
    LinkClicked(String),
    /// A block kind was picked (index into [`BLOCKS`](crate::ui::BLOCKS)).
    Block(usize),
    /// A font family was picked (index into [`FAMILIES`](crate::ui::FAMILIES)).
    Family(usize),
    /// A font size was picked (index into [`SIZES`](crate::ui::SIZES)).
    Size(usize),
    Toggle(Mark),
    Align(Align),
    List(ListKind),
    Indent,
    Outdent,
    /// A wrap was picked for the selected image (index into the list).
    Wrap(usize),
    /// Ctrl+Enter from the toolbar: the rest of the paragraph starts a page.
    PageBreak,
    /// The Page view toggle: page view when on, draft view when off.
    PageView(bool),
    /// The Page setup button: show its menu.
    PageSetup,
    /// An entry of the Page setup menu was picked.
    PageChoice(usize),
    /// The Table button: show its menu.
    Table,
    /// An entry of the Table menu was picked, with a switch's new state.
    TableChoice(usize, bool),
    /// The Save / Discard / Cancel prompt was dismissed.
    Unsaved(TaskDialogAction),
    /// The error message was dismissed.
    MessageClosed,
    /// A file picker was cancelled.
    PickerClosed,
    /// Ctrl+Q or the window's close button.
    Quit,
}

/// What to do once unsaved changes are saved or discarded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum After {
    New,
    Open,
    Quit,
}

/// The modal dialogs and file pickers, held for the window's lifetime.
pub struct Dialogs {
    pub open: FileDialog<Msg>,
    pub save: FileDialog<Msg>,
    pub export: FileDialog<Msg>,
    pub image: FileDialog<Msg>,
    /// Save / Discard / Cancel for a modified document.
    pub unsaved: TaskDialog<Msg>,
    /// The link address prompt.
    pub link: Dialog<Msg>,
    /// An error message.
    pub message: Dialog<Msg>,
    /// The Page setup menu: paper, orientation, margins.
    pub page: Menu<Msg>,
    /// The Table menu: insert a table, edit rows and columns, switches.
    pub table: Menu<Msg>,
}

/// The LazyWriter application.
pub struct Writer {
    pub editor: Rc<RichTextEditor<Msg>>,
    pub tools: Tools,
    pub status: Rc<StatusBar<Msg>>,
    pub dialogs: Dialogs,
    pub host: Host,
    /// The file the document was opened from or saved to, if any.
    pub path: Option<PathBuf>,
    /// Whether the document changed since it was opened, saved or created.
    pub dirty: bool,
    /// The last formatting summary, re-applied to the toolbar after commands.
    pub summary: Option<StyleSummary>,
    /// What runs once unsaved changes are dealt with (kept through Save As).
    pub after: Option<After>,
    /// Whether a dialog or picker is open, so shortcuts leave it alone. The
    /// window's key hook shares it.
    pub dialog_open: Rc<Cell<bool>>,
    pub _mounted: Mounted<Msg>,
}

impl Writer {
    /// Whether the document has unsaved changes.
    pub fn is_modified(&self) -> bool {
        self.dirty
    }

    /// The file the document belongs to.
    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }
}

/// Maps Ctrl+N / O / S / Shift+S / E / Q to messages. The editor handles
/// Ctrl+B / I / U / Z / Y / X / C / V / A itself. While a dialog is open no
/// key is taken, so Esc and Enter reach the dialog, not the document.
pub fn shortcut(key: Key, modifiers: Modifiers, dialog_open: &Cell<bool>) -> Option<Msg> {
    if dialog_open.get() || !(modifiers.ctrl || modifiers.win) || modifiers.alt {
        return None;
    }
    match key {
        Key::N => Some(Msg::New),
        Key::O => Some(Msg::Open),
        Key::S if modifiers.shift => Some(Msg::SaveAs),
        Key::S => Some(Msg::Save),
        Key::E => Some(Msg::Export),
        Key::Q => Some(Msg::Quit),
        _ => None,
    }
}

impl App for Writer {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::New => files::guard(self, ui, After::New),
            Msg::Open => files::guard(self, ui, After::Open),
            Msg::Quit => files::guard(self, ui, After::Quit),
            Msg::OpenChosen(path) => files::open_chosen(self, ui, path),
            Msg::Save => files::save(self, ui),
            Msg::SaveAs => files::save_as(self),
            Msg::SaveChosen(path) => files::save_chosen(self, ui, path),
            Msg::Export => files::export(self),
            Msg::ExportChosen(path) => files::export_chosen(self, path),
            Msg::InsertImage => files::insert_image(self),
            Msg::ImageChosen(path) => files::image_chosen(self, path),
            Msg::Unsaved(action) => files::unsaved(self, ui, action),
            Msg::MessageClosed | Msg::PickerClosed => files::dialog_closed(self, ui),
            Msg::Link => commands::link(self),
            Msg::LinkChosen(action) => commands::link_chosen(self, action),
            Msg::Undo => commands::format(self, Command::Undo),
            Msg::Redo => commands::format(self, Command::Redo),
            Msg::Cut => commands::format(self, Command::Cut),
            Msg::Copy => commands::format(self, Command::Copy),
            Msg::Paste => commands::format(self, Command::Paste),
            Msg::Edited(words) => commands::edited(self, ui, words),
            Msg::Selection(summary) => commands::selection(self, summary),
            Msg::LinkClicked(url) => self.status.set_text(0, &url),
            Msg::Block(index) => commands::block(self, index),
            Msg::Family(index) => commands::family(self, index),
            Msg::Size(index) => commands::size(self, index),
            Msg::Toggle(mark) => commands::toggle(self, mark),
            Msg::Align(align) => commands::format(self, Command::SetAlign(align)),
            Msg::List(kind) => commands::format(self, Command::ToggleList(kind)),
            Msg::Indent => commands::format(self, Command::Indent),
            Msg::Outdent => commands::format(self, Command::Outdent),
            Msg::Wrap(index) => commands::wrap(self, index),
            Msg::PageBreak => commands::format(self, Command::InsertPageBreak),
            Msg::PageView(on) => commands::page_view(self, on),
            Msg::PageSetup => commands::page_menu(self, ui),
            Msg::PageChoice(id) => commands::page_choice(self, id),
            Msg::Table => commands::table_menu(self, ui),
            Msg::TableChoice(index, on) => commands::table_choice(self, index, on),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: Modifiers = Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    };

    fn key(key: Key, modifiers: Modifiers) -> Option<Msg> {
        shortcut(key, modifiers, &Cell::new(false))
    }

    #[test]
    fn the_file_shortcuts_map_to_their_commands() {
        assert!(matches!(key(Key::N, CTRL), Some(Msg::New)));
        assert!(matches!(key(Key::O, CTRL), Some(Msg::Open)));
        assert!(matches!(key(Key::S, CTRL), Some(Msg::Save)));
        let shift = Modifiers {
            shift: true,
            ..CTRL
        };
        assert!(matches!(key(Key::S, shift), Some(Msg::SaveAs)));
        assert!(matches!(key(Key::E, CTRL), Some(Msg::Export)));
        assert!(matches!(key(Key::Q, CTRL), Some(Msg::Quit)));
    }

    #[test]
    fn editing_keys_are_left_to_the_editor() {
        for k in [
            Key::B,
            Key::I,
            Key::U,
            Key::Z,
            Key::Y,
            Key::X,
            Key::C,
            Key::V,
        ] {
            assert!(key(k, CTRL).is_none());
        }
        assert!(key(Key::S, Modifiers::NONE).is_none(), "plain typing");
        let altgr = Modifiers { alt: true, ..CTRL };
        assert!(key(Key::S, altgr).is_none(), "AltGr is typing too");
    }

    #[test]
    fn an_open_dialog_takes_no_shortcut() {
        let open = Cell::new(true);
        assert!(shortcut(Key::S, CTRL, &open).is_none());
        assert!(shortcut(Key::Q, CTRL, &open).is_none());
    }
}
