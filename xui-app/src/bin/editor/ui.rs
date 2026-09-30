#![forbid(unsafe_code)]

//! Builds the notepad's widget tree: a menu bar, the find bar, the editor and a
//! status bar, laid out with `xui_core::arrange` so the window resizes cleanly.

use std::cell::Cell;
use std::rc::Rc;

use xui_code_editor::{Editor, FontConfig, Options};
use xui_core::app::Ui;
use xui_core::arrange::{column, row, widget, LayoutExt};
use xui_core::backend::{Result, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::widget::{
    Button, CheckBox, Dialog, Edit, FileDialog, Label, Menu, MenuId, Placeable, StatusBar,
};
use xui_core::Dip;

use crate::app::{FindBar, Msg, Notepad};

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

/// A menu bar as a layout entry: it has a fixed design height and fills the
/// window's width.
struct MenuPane<M: 'static>(Menu<M>);

impl<M: 'static> Placeable<M> for MenuPane<M> {
    fn id(&self) -> WidgetId {
        self.0.id().expect("a menu bar owns a node")
    }

    fn natural_size(&self, _ui: &Ui<M>, dpi: u32) -> Size {
        Size::new(0, MENU_HEIGHT.to_px(dpi).value())
    }
}

/// The editor as a layout entry: it takes all the leftover space.
struct EditorPane<M: 'static>(Rc<Editor<M>>);

impl<M: 'static> Placeable<M> for EditorPane<M> {
    fn id(&self) -> WidgetId {
        self.0.id()
    }

    fn natural_size(&self, _ui: &Ui<M>, _dpi: u32) -> Size {
        Size::new(0, 0)
    }
}

/// Builds the app's widgets and mounts the layout.
pub fn build(ui: &Ui<Msg>) -> Result<Notepad> {
    // The editor is a monospace grid: the default UI font is proportional and
    // would space the glyphs apart.
    let options = Options {
        font: FontConfig {
            family: Some(xui_app::font::MONO_FAMILY.to_owned()),
            ..FontConfig::default()
        },
        ..Options::default()
    };
    let editor = Rc::new(
        Editor::with_options(ui, Rect::default(), options)?.on_change(|_text| Some(Msg::Edited)),
    );

    let menu = Menu::bar(ui, Rect::default())?
        .on_select(menu_msg)
        .build(|bar| {
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
        });

    let find_bar = build_find_bar(ui)?;
    find_bar.set_visible(ui, false);

    let status = Rc::new(StatusBar::auto(
        ui,
        &["Ln 1, Col 1", "Sel 0", "LF", "Saved"],
    )?);

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

    let root = column()
        .child(widget(MenuPane(menu)).height(MENU_HEIGHT))
        .child(
            row()
                .spacing(Dip(4.0))
                .child(&find_bar.query)
                .child(&find_bar.replacement)
                .child(&find_bar.buttons[0])
                .child(&find_bar.buttons[1])
                .child(&find_bar.buttons[2])
                .child(&find_bar.buttons[3])
                .child(&find_bar.regex)
                .child(&find_bar.case)
                .child(&find_bar.status)
                .fixed(FIND_HEIGHT),
        )
        .child(widget(EditorPane(Rc::clone(&editor))).fill(1))
        .child(&status);
    let mounted = ui.mount(root)?;

    editor.focus();

    Ok(Notepad {
        editor,
        document: xui_code_editor::Document::untitled(),
        search: Default::default(),
        find_bar,
        status,
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

/// The find/replace bar's widgets, wired to their messages.
fn build_find_bar(ui: &Ui<Msg>) -> Result<FindBar> {
    let query = Rc::new(
        Edit::auto(ui, "")?
            .cue("Find")
            .on_change(|text| Some(Msg::QueryChanged(text.to_string()))),
    );
    let replacement = Rc::new(Edit::auto(ui, "")?.cue("Replace"));
    let status = Rc::new(Label::auto(ui, "")?);
    let regex = Rc::new(CheckBox::auto(ui, "Regex")?.on_toggle(|on| Some(Msg::RegexToggled(on))));
    let case =
        Rc::new(CheckBox::auto(ui, "Match case")?.on_toggle(|on| Some(Msg::CaseToggled(on))));
    let next = Rc::new(Button::auto(ui, "Next")?.on_click(|| Some(Msg::FindNext)));
    let prev = Rc::new(Button::auto(ui, "Previous")?.on_click(|| Some(Msg::FindPrevious)));
    let replace = Rc::new(Button::auto(ui, "Replace")?.on_click(|| Some(Msg::ReplaceCurrent)));
    let replace_all = Rc::new(Button::auto(ui, "Replace all")?.on_click(|| Some(Msg::ReplaceAll)));

    let nodes = [
        query.id(),
        replacement.id(),
        next.id(),
        prev.id(),
        replace.id(),
        replace_all.id(),
        regex.id(),
        case.id(),
        status.id(),
    ]
    .to_vec();

    Ok(FindBar {
        query,
        replacement,
        status,
        regex,
        case,
        buttons: [next, prev, replace, replace_all],
        nodes,
    })
}

/// Maps a dialog dismissal to a message.
fn dialog_msg(action: xui_core::widget::DialogAction) -> Option<Msg> {
    Some(Msg::Dialog(action))
}
