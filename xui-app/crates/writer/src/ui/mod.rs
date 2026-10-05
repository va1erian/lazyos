#![forbid(unsafe_code)]

//! Builds LazyWriter's widget tree, ported from the wordpad example's
//! `ui.rs`: a command toolbar, a formatting row, the editor and a status bar,
//! laid out with `xui_core::arrange`. Every toolbar action has a Lucide icon.

mod dialogs;
pub mod page_menu;
pub mod print_bar;
pub mod table_menu;
mod tools;

use std::cell::Cell;
use std::rc::Rc;

use xui_core::Dip;
use xui_core::app::Ui;
use xui_core::arrange::{Handle, LayoutExt, build as create, column, status_bar};
use xui_core::backend::{Result, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::widget::{Lucide, Placeable, Toolbar};
use xui_rich_text::{RichTextEditor, ViewMode};

use crate::app::{Msg, Writer, shortcut};
use crate::files::word_count;
use crate::host::Host;

pub use tools::{
    ALIGNS, BLOCK_ICONS, BLOCKS, DEFAULT_SIZE, FAMILIES, SIZES, Tips, Tools, WRAPS, family_index,
};

const TOOLBAR_HEIGHT: Dip = Dip(36.0);
const FORMAT_HEIGHT: Dip = Dip(34.0);

/// The toolbar's commands, in the order of its items.
const COMMANDS: [fn() -> Msg; 13] = [
    || Msg::New,
    || Msg::Open,
    || Msg::Save,
    || Msg::Export,
    || Msg::Print,
    || Msg::Undo,
    || Msg::Redo,
    || Msg::Cut,
    || Msg::Copy,
    || Msg::Paste,
    || Msg::InsertImage,
    || Msg::Link,
    || Msg::PageBreak,
];

/// The toolbar as a layout entry.
struct ToolbarPane(Toolbar<Msg>);

impl Placeable<Msg> for ToolbarPane {
    fn id(&self) -> WidgetId {
        self.0.id()
    }

    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }
}

/// The command toolbar: files, history, clipboard, insert.
fn command_toolbar(ui: &Ui<Msg>) -> Result<ToolbarPane> {
    let toolbar = Toolbar::empty(ui, Rect::default())?
        .item_with_text(Lucide::FilePlus, "New (Ctrl+N)", "New")
        .item_with_text(Lucide::FolderOpen, "Open (Ctrl+O)", "Open")
        .item_with_text(Lucide::Save, "Save (Ctrl+S)", "Save")
        .item_with_text(Lucide::Download, "Export as Markdown (Ctrl+E)", "Export")
        .item_with_text(Lucide::Printer, "Print (Ctrl+P)", "Print")
        .separator()
        .item_with_text(Lucide::Undo2, "Undo (Ctrl+Z)", "Undo")
        .item_with_text(Lucide::Redo2, "Redo (Ctrl+Y)", "Redo")
        .separator()
        .item_with_text(Lucide::Scissors, "Cut (Ctrl+X)", "Cut")
        .item_with_text(Lucide::Copy, "Copy (Ctrl+C)", "Copy")
        .item_with_text(Lucide::ClipboardPaste, "Paste (Ctrl+V)", "Paste")
        .separator()
        .item_with_text(Lucide::Image, "Insert image", "Image")
        .item_with_text(Lucide::Link, "Link the selection", "Link")
        .item_with_text(
            Lucide::SeparatorHorizontal,
            "Page break (Ctrl+Enter)",
            "Page break",
        )
        .on_click(|index| COMMANDS.get(index).map(|msg| msg()));
    Ok(ToolbarPane(toolbar))
}

/// The rich-text editor, in page view.
fn new_editor(ui: &Ui<Msg>) -> Result<RichTextEditor<Msg>> {
    Ok(RichTextEditor::new(ui, Rect::default())?
        .on_change(|doc| Some(Msg::Edited(word_count(doc))))
        .on_selection(|summary| Some(Msg::Selection(summary.clone())))
        .on_link(|url| Some(Msg::LinkClicked(url.to_owned())))
        .view_mode(ViewMode::Page))
}

/// Builds the app's widgets, wires the window's keys and close button, and
/// mounts the layout.
pub fn build(ui: &Ui<Msg>, host: Host) -> Result<Writer> {
    let tools = Tools::default();
    let print_bar = print_bar::PrintBar::default();
    let editor = Handle::new();
    let status = Handle::new();
    let dialogs = dialogs::build(ui, &host)?;
    let mounted = ui.mount(column().children((
        create(command_toolbar).height(TOOLBAR_HEIGHT),
        tools.row().fixed(FORMAT_HEIGHT),
        create(new_editor).bind(&editor).fill(1),
        print_bar.row().fixed(FORMAT_HEIGHT),
        status_bar(&["Untitled", "Saved", "0 words", "Page 1 of 1", ""]).bind(&status),
    )))?;
    let editor = editor.get();
    editor.focus();

    let dialog_open = Rc::new(Cell::new(false));
    ui.on_close(|| Some(Msg::Quit));
    ui.on_timer(|_| Some(Msg::PrintTick));
    {
        let dialog_open = Rc::clone(&dialog_open);
        ui.on_key(move |key, modifiers| shortcut(key, modifiers, &dialog_open));
    }

    let app = Writer {
        editor,
        tools,
        status: status.get(),
        dialogs,
        print_bar,
        printing: None,
        host,
        path: None,
        dirty: false,
        summary: None,
        after: None,
        dialog_open,
        _mounted: mounted,
    };
    crate::commands::refresh_title(&app, ui);
    Ok(app)
}
