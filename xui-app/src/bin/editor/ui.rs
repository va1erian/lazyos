#![forbid(unsafe_code)]

//! Builds the notepad's widget tree: a menu bar, the find bar, the editor and a
//! status bar, laid out with `xui_core::arrange` so the window resizes cleanly.

use std::cell::Cell;
use std::rc::Rc;

use xui_code_editor::{Editor, FontConfig, Options};
use xui_core::app::Ui;
use xui_core::arrange::{
    build as create, button, checkbox, column, edit, label, menu_bar, row, status_bar, Build,
    Handle, Layout, LayoutExt,
};
use xui_core::backend::{Result, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::widget::{
    Button, CheckBox, Dialog, Edit, FileDialog, Label, Menu, MenuId, Placeable,
};
use xui_core::Dip;

use xui_core::widget::StdFileSystem;

use crate::app::{FindBar, Msg, Notepad};

/// Where the pickers start for an untitled document: `/tmp`, the one writable
/// volume every LazyOS boot has (the FAT root may be read-only media).
const START_DIR: &str = fhs::mount::TMP;
/// The menu bar's design height.
const MENU_HEIGHT: Dip = Dip(30.0);
/// The find bar's design height.
const FIND_HEIGHT: Dip = Dip(34.0);

// File menu commands.
const NEW: MenuId = MenuId::new(1);
const OPEN: MenuId = MenuId::new(2);
const SAVE: MenuId = MenuId::new(3);
const SAVE_AS: MenuId = MenuId::new(4);
const QUIT: MenuId = MenuId::new(5);
// Edit menu commands.
const UNDO: MenuId = MenuId::new(11);
const REDO: MenuId = MenuId::new(12);
const CUT: MenuId = MenuId::new(13);
const COPY: MenuId = MenuId::new(14);
const PASTE: MenuId = MenuId::new(15);
const SELECT_ALL: MenuId = MenuId::new(16);
const FIND: MenuId = MenuId::new(17);
const REPLACE: MenuId = MenuId::new(18);

/// Maps a chosen menu command to a message.
fn menu_msg(id: MenuId) -> Option<Msg> {
    Some(match id {
        NEW => Msg::New,
        OPEN => Msg::Open,
        SAVE => Msg::Save,
        SAVE_AS => Msg::SaveAs,
        QUIT => Msg::Quit,
        UNDO => Msg::Undo,
        REDO => Msg::Redo,
        CUT => Msg::Cut,
        COPY => Msg::Copy,
        PASTE => Msg::Paste,
        SELECT_ALL => Msg::SelectAll,
        FIND => Msg::Find,
        REPLACE => Msg::Replace,
        _ => return None,
    })
}

/// The editor as a layout entry: it takes all the leftover space.
struct EditorPane<M: 'static>(Rc<Editor<M>>);

impl<M: 'static> Placeable<M> for EditorPane<M> {
    fn id(&self) -> WidgetId {
        self.0.id()
    }

    fn measure(&self, _ui: &Ui<M>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }

    fn placed(&self, _ui: &Ui<M>, _rect: Rect) {
        // The mounted layout re-flows the editor node on a window resize; keep
        // the caret and scroll valid for the new, possibly narrower, viewport.
        self.0.on_resize();
    }
}

/// The menu bar.
fn menus() -> Build<Menu<Msg>, Msg> {
    menu_bar(|bar| {
        bar.submenu(MenuId::new(100), "&File", |file| {
            file.item(NEW, "&New");
            file.item(OPEN, "&Open...");
            file.separator();
            file.item(SAVE, "&Save");
            file.item(SAVE_AS, "Save &As...");
            file.separator();
            file.item(QUIT, "&Quit");
        });
        bar.submenu(MenuId::new(200), "&Edit", |edit| {
            edit.item(UNDO, "&Undo");
            edit.item(REDO, "&Redo");
            edit.separator();
            edit.item(CUT, "Cu&t");
            edit.item(COPY, "&Copy");
            edit.item(PASTE, "&Paste");
            edit.item(SELECT_ALL, "Select &All");
            edit.separator();
            edit.item(FIND, "&Find...");
            edit.item(REPLACE, "&Replace...");
        });
    })
    .on_select_with(menu_msg)
}

/// The code editor, in the monospace UI font.
fn new_editor(ui: &Ui<Msg>) -> Result<EditorPane<Msg>> {
    // The editor is a monospace grid: the default UI font is proportional and
    // would space the glyphs apart.
    let options = Options {
        font: FontConfig {
            family: Some(xui_app::font::MONO_FAMILY.to_owned()),
            ..FontConfig::default()
        },
        ..Options::default()
    };
    let editor =
        Editor::with_options(ui, Rect::default(), options)?.on_change(|_text| Some(Msg::Edited));
    Ok(EditorPane(Rc::new(editor)))
}

/// Builds the app's widgets and mounts the layout.
pub fn build(ui: &Ui<Msg>) -> Result<Notepad> {
    let editor = Handle::new();
    let find = FindHandles::default();
    let status = Handle::new();
    let mounted = ui.mount(column().children((
        menus().height(MENU_HEIGHT),
        find.row().fixed(FIND_HEIGHT),
        create(new_editor).bind(&editor).fill(1),
        status_bar(&["Ln 1, Col 1", "Sel 0", "LF", "Saved"]).bind(&status),
    )))?;
    let editor = Rc::clone(&editor.get().0);
    let find_bar = find.get();
    find_bar.set_visible(ui, false);

    let confirm = Dialog::confirm(
        ui,
        "Discard unsaved changes?",
        "This document has unsaved changes. Discard them?",
    )?
    .on_action(dialog_msg);
    let message = Dialog::message(ui, "Error", "")?.on_action(dialog_msg);

    let dialog_open = Rc::new(Cell::new(false));
    let find_open = Rc::new(Cell::new(false));

    let cancel = {
        let editor = Rc::clone(&editor);
        let dialog_open = Rc::clone(&dialog_open);
        move || {
            dialog_open.set(false);
            editor.focus();
            None
        }
    };
    let open_dialog = FileDialog::open_file(ui, "Open")?
        .file_system(Rc::new(StdFileSystem))
        .initial_dir(START_DIR)
        .require_existing(true)
        .on_accept(|path| Some(Msg::OpenChosen(path)))
        .on_cancel(cancel);
    let cancel = {
        let editor = Rc::clone(&editor);
        let dialog_open = Rc::clone(&dialog_open);
        move || {
            dialog_open.set(false);
            editor.focus();
            None
        }
    };
    let save_dialog = FileDialog::save_file(ui, "Save As")?
        .file_system(Rc::new(StdFileSystem))
        .initial_dir(START_DIR)
        .filter("Text files", &["txt", "md", "rs"])
        .filter("All files", &[])
        .on_accept(|path| Some(Msg::SaveChosen(path)))
        .on_cancel(cancel);
    ui.on_close(|| Some(Msg::CloseRequested));
    {
        let dialog_open = Rc::clone(&dialog_open);
        let find_open = Rc::clone(&find_open);
        ui.on_key(move |key, modifiers| {
            crate::app::shortcut(key, modifiers, &dialog_open, &find_open)
        });
    }

    editor.focus();

    Ok(Notepad {
        editor,
        document: xui_code_editor::Document::untitled(),
        search: Default::default(),
        find_bar,
        status: status.get(),
        open_dialog,
        save_dialog,
        confirm,
        message,
        pending: crate::app::Pending::None,
        dialog_open,
        find_open,
        _mounted: mounted,
    })
}

/// The find/replace bar's widgets, filled when its row is mounted.
#[derive(Default)]
struct FindHandles {
    query: Handle<Edit<Msg>>,
    replacement: Handle<Edit<Msg>>,
    status: Handle<Label<Msg>>,
    regex: Handle<CheckBox<Msg>>,
    case: Handle<CheckBox<Msg>>,
    buttons: [Handle<Button<Msg>>; 4],
}

impl FindHandles {
    /// The find/replace bar, wired to its messages.
    fn row(&self) -> Layout<Msg> {
        let [next, prev, replace, replace_all] = &self.buttons;
        row().gap(4).children((
            edit()
                .placeholder("Find")
                .bind(&self.query)
                .on_change(Msg::QueryChanged),
            edit().placeholder("Replace").bind(&self.replacement),
            button("Next")
                .bind(next)
                .on_click_with(|| Some(Msg::FindNext)),
            button("Previous")
                .bind(prev)
                .on_click_with(|| Some(Msg::FindPrevious)),
            button("Replace")
                .bind(replace)
                .on_click_with(|| Some(Msg::ReplaceCurrent)),
            button("Replace all")
                .bind(replace_all)
                .on_click_with(|| Some(Msg::ReplaceAll)),
            checkbox("Regex")
                .bind(&self.regex)
                .on_toggle(Msg::RegexToggled),
            checkbox("Match case")
                .bind(&self.case)
                .on_toggle(Msg::CaseToggled),
            label("").bind(&self.status),
        ))
    }

    /// The mounted bar.
    fn get(&self) -> FindBar {
        let buttons = self.buttons.each_ref().map(Handle::get);
        let (query, replacement) = (self.query.get(), self.replacement.get());
        let (regex, case, status) = (self.regex.get(), self.case.get(), self.status.get());
        let mut nodes = vec![query.id(), replacement.id()];
        nodes.extend(buttons.iter().map(|button| button.id()));
        nodes.extend([regex.id(), case.id(), status.id()]);
        FindBar {
            query,
            replacement,
            status,
            regex,
            case,
            nodes,
        }
    }
}

/// Maps a dialog dismissal to a message.
fn dialog_msg(action: xui_core::widget::DialogAction) -> Option<Msg> {
    Some(Msg::Dialog(action))
}
