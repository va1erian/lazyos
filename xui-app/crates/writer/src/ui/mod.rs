#![forbid(unsafe_code)]

//! Builds LazyWriter's widget tree, ported from the wordpad example's
//! `ui.rs`: a command toolbar, a formatting row, the editor and a status bar,
//! laid out with `xui_core::arrange`. Every toolbar action has a Lucide icon.

mod dialogs;
pub mod page_menu;
mod tools;

use std::cell::Cell;
use std::rc::Rc;

use xui_core::Dip;
use xui_core::app::Ui;
use xui_core::arrange::{Handle, LayoutExt, build as build_with, column, status_bar};
use xui_core::backend::{Result, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::widget::{Lucide, Placeable, Toolbar};
use xui_rich_text::{RichTextEditor, ViewMode};

use crate::app::{Msg, Writer, shortcut};
use crate::files::word_count;
use crate::host::Host;

use tools::ToolHandles;
pub use tools::{
    ALIGNS, BLOCK_ICONS, BLOCKS, DEFAULT_SIZE, FAMILIES, SIZES, Tools, WRAPS, family_index,
};

const TOOLBAR_HEIGHT: Dip = Dip(36.0);
const FORMAT_HEIGHT: Dip = Dip(34.0);

/// The toolbar's commands, in the order of its items.
const COMMANDS: [fn() -> Msg; 12] = [
    || Msg::New,
    || Msg::Open,
    || Msg::Save,
    || Msg::Export,
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

/// The shared editor as a layout entry: it takes the leftover space.
struct EditorPane(Rc<RichTextEditor<Msg>>);

impl Placeable<Msg> for EditorPane {
    fn id(&self) -> WidgetId {
        self.0.id()
    }

    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }
}

/// The command toolbar: files, history, clipboard, insert.
fn command_toolbar(ui: &Ui<Msg>) -> Result<ToolbarPane> {
    Ok(ToolbarPane(
        Toolbar::empty(ui, Rect::default())?
            .item_with_text(Lucide::FilePlus, "New (Ctrl+N)", "New")
            .item_with_text(Lucide::FolderOpen, "Open (Ctrl+O)", "Open")
            .item_with_text(Lucide::Save, "Save (Ctrl+S)", "Save")
            .item_with_text(Lucide::Download, "Export as Markdown (Ctrl+E)", "Export")
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
            .on_click(|index| COMMANDS.get(index).map(|msg| msg())),
    ))
}

/// The editor, in page view, wired to its messages.
fn new_editor(ui: &Ui<Msg>) -> Result<EditorPane> {
    let editor = RichTextEditor::new(ui, Rect::default())?
        .on_change(|doc| Some(Msg::Edited(word_count(doc))))
        .on_selection(|summary| Some(Msg::Selection(summary.clone())))
        .on_link(|url| Some(Msg::LinkClicked(url.to_owned())))
        .view_mode(ViewMode::Page);
    Ok(EditorPane(Rc::new(editor)))
}

/// Builds the app's widgets, wires the window's keys and close button, and
/// mounts the layout.
pub fn build(ui: &Ui<Msg>, host: Host) -> Result<Writer> {
    let tools = ToolHandles::default();
    let editor = Handle::new();
    let status = Handle::new();
    let mounted = ui.mount(column().children((
        build_with(command_toolbar).height(TOOLBAR_HEIGHT),
        tools.row().fixed(FORMAT_HEIGHT),
        build_with(new_editor).bind(&editor).fill(1),
        status_bar(&["Untitled", "Saved", "0 words", "Page 1 of 1"]).bind(&status),
    )))?;
    let editor = Rc::clone(&editor.get().0);
    let tools = tools.tools();
    let status = status.get();
    let dialogs = dialogs::build(ui, &host)?;

    let dialog_open = Rc::new(Cell::new(false));
    ui.on_close(|| Some(Msg::Quit));
    {
        let dialog_open = Rc::clone(&dialog_open);
        ui.on_key(move |key, modifiers| shortcut(key, modifiers, &dialog_open));
    }

    editor.focus();

    let app = Writer {
        editor,
        tools,
        status,
        dialogs,
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
