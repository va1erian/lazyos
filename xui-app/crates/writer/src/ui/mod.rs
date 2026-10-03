#![forbid(unsafe_code)]

//! Builds LazyWriter's widget tree, ported from the wordpad example's
//! `ui.rs`: a command toolbar, a formatting row, the editor and a status bar,
//! laid out with `xui_core::arrange`. Every toolbar action has a Lucide icon.

mod dialogs;
mod tools;

use std::cell::Cell;
use std::rc::Rc;

use xui_core::Dip;
use xui_core::app::Ui;
use xui_core::arrange::{LayoutExt, column, row, spacer, widget};
use xui_core::backend::{Result, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Insets;
use xui_core::widget::{Lucide, Placeable, StatusBar, Toolbar};
use xui_rich_text::RichTextEditor;
use xui_rich_text::model::{Align, ListKind};

use crate::app::{Mark, Msg, Writer, shortcut};
use crate::files::word_count;
use crate::host::Host;

pub use tools::{
    ALIGNS, BLOCK_ICONS, BLOCKS, DEFAULT_SIZE, FAMILIES, SIZES, Tools, WRAPS, family_index,
};
use tools::{picker, push, toggle};

const TOOLBAR_HEIGHT: Dip = Dip(36.0);
const FORMAT_HEIGHT: Dip = Dip(34.0);
/// The width of an icon-only button in the formatting row.
const ICON_WIDTH: Dip = Dip(32.0);

/// The toolbar's commands, in the order of its items.
const COMMANDS: [fn() -> Msg; 11] = [
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
];

/// The toolbar as a layout entry.
struct ToolbarPane(Toolbar<Msg>);

impl Placeable<Msg> for ToolbarPane {
    fn id(&self) -> WidgetId {
        self.0.id()
    }

    fn natural_size(&self, _ui: &Ui<Msg>, _dpi: u32) -> Size {
        Size::new(0, 0)
    }
}

/// The shared editor as a layout entry: it takes the leftover space.
struct EditorPane(Rc<RichTextEditor<Msg>>);

impl Placeable<Msg> for EditorPane {
    fn id(&self) -> WidgetId {
        self.0.id()
    }

    fn natural_size(&self, _ui: &Ui<Msg>, _dpi: u32) -> Size {
        Size::new(0, 0)
    }
}

/// The command toolbar: files, history, clipboard, insert.
fn command_toolbar(ui: &Ui<Msg>) -> Result<Toolbar<Msg>> {
    Ok(Toolbar::empty(ui, Rect::default())?
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
        .on_click(|index| COMMANDS.get(index).map(|msg| msg())))
}

/// The formatting row's controls.
fn format_tools(ui: &Ui<Msg>) -> Result<(Tools, [xui_core::widget::Button<Msg>; 2])> {
    let block = picker(ui, &BLOCKS, Msg::Block)?;
    for (index, icon) in BLOCK_ICONS.into_iter().enumerate() {
        block.set_item_icon(index, Some(icon.into()));
    }
    let family = picker(ui, &FAMILIES, Msg::Family)?;
    let size_labels: Vec<String> = SIZES.iter().map(|s| format!("{s}")).collect();
    let size_refs: Vec<&str> = size_labels.iter().map(String::as_str).collect();
    let size = picker(ui, &size_refs, Msg::Size)?;
    size.select(DEFAULT_SIZE);
    let wrap = picker(ui, &WRAPS, Msg::Wrap)?;
    wrap.set_enabled(false);

    let mut tips = Vec::new();
    let t = &mut tips;
    let marks = [
        toggle(ui, t, (Lucide::Bold, "Bold (Ctrl+B)"), || {
            Msg::Toggle(Mark::Bold)
        })?,
        toggle(ui, t, (Lucide::Italic, "Italic (Ctrl+I)"), || {
            Msg::Toggle(Mark::Italic)
        })?,
        toggle(ui, t, (Lucide::Underline, "Underline (Ctrl+U)"), || {
            Msg::Toggle(Mark::Underline)
        })?,
        toggle(ui, t, (Lucide::Strikethrough, "Strikethrough"), || {
            Msg::Toggle(Mark::Strike)
        })?,
    ];
    let aligns = [
        toggle(ui, t, (Lucide::TextAlignStart, "Align left"), || {
            Msg::Align(Align::Left)
        })?,
        toggle(ui, t, (Lucide::TextAlignCenter, "Centre"), || {
            Msg::Align(Align::Center)
        })?,
        toggle(ui, t, (Lucide::TextAlignEnd, "Align right"), || {
            Msg::Align(Align::Right)
        })?,
        toggle(ui, t, (Lucide::TextAlignJustify, "Justify"), || {
            Msg::Align(Align::Justify)
        })?,
    ];
    let lists = [
        toggle(ui, t, (Lucide::List, "Bulleted list"), || {
            Msg::List(ListKind::Bullet)
        })?,
        toggle(ui, t, (Lucide::ListOrdered, "Numbered list"), || {
            Msg::List(ListKind::Numbered)
        })?,
    ];
    let indent = push(ui, t, (Lucide::IndentIncrease, "Indent"), || Msg::Indent)?;
    let outdent = push(ui, t, (Lucide::IndentDecrease, "Outdent"), || Msg::Outdent)?;
    let tools = Tools {
        block,
        family,
        size,
        marks,
        aligns,
        lists,
        wrap,
        tips,
    };
    Ok((tools, [indent, outdent]))
}

/// Builds the app's widgets, wires the window's keys and close button, and
/// mounts the layout.
pub fn build(ui: &Ui<Msg>, host: Host) -> Result<Writer> {
    let editor = Rc::new(
        RichTextEditor::new(ui, Rect::default())?
            .on_change(|doc| Some(Msg::Edited(word_count(doc))))
            .on_selection(|summary| Some(Msg::Selection(summary.clone())))
            .on_link(|url| Some(Msg::LinkClicked(url.to_owned()))),
    );
    let commands = command_toolbar(ui)?;
    let (tools, [indent, outdent]) = format_tools(ui)?;
    let status = Rc::new(StatusBar::auto(ui, &["Untitled", "Saved", "0 words"])?);
    let dialogs = dialogs::build(ui, &host)?;

    let dialog_open = Rc::new(Cell::new(false));
    ui.on_close(|| Some(Msg::Quit));
    {
        let dialog_open = Rc::clone(&dialog_open);
        ui.on_key(move |key, modifiers| shortcut(key, modifiers, &dialog_open));
    }

    let gap = Dip(6.0);
    let t = &tools;
    let root = column()
        .child(widget(ToolbarPane(commands)).height(TOOLBAR_HEIGHT))
        .child(
            row()
                .spacing(Dip(4.0))
                .margins(Insets::symmetric(Dip(8.0), Dip(3.0)))
                .child((&t.block).width(Dip(120.0)))
                .child((&t.family).width(Dip(96.0)))
                .child((&t.size).width(Dip(64.0)))
                .child(spacer().width(gap))
                .child((&t.marks[0]).width(ICON_WIDTH))
                .child((&t.marks[1]).width(ICON_WIDTH))
                .child((&t.marks[2]).width(ICON_WIDTH))
                .child((&t.marks[3]).width(ICON_WIDTH))
                .child(spacer().width(gap))
                .child((&t.aligns[0]).width(ICON_WIDTH))
                .child((&t.aligns[1]).width(ICON_WIDTH))
                .child((&t.aligns[2]).width(ICON_WIDTH))
                .child((&t.aligns[3]).width(ICON_WIDTH))
                .child(spacer().width(gap))
                .child((&t.lists[0]).width(ICON_WIDTH))
                .child((&t.lists[1]).width(ICON_WIDTH))
                .child(indent.width(ICON_WIDTH))
                .child(outdent.width(ICON_WIDTH))
                .child(spacer().width(gap))
                .child((&t.wrap).width(Dip(120.0)))
                .child(spacer())
                .fixed(FORMAT_HEIGHT),
        )
        .child(widget(EditorPane(Rc::clone(&editor))).fill(1))
        .child(&status);
    let mounted = ui.mount(root)?;
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
