//! The window's widgets and layout: a command toolbar with the compression
//! level, the address line, the report list (or the welcome line when no
//! archive is open), the progress strip of a running job, and the status
//! bar. Every dialog is built once, hidden, and kept for the window's life.

use std::rc::Rc;

use lazyarc::format::{Format, Level, ALL as FORMATS};
use xui_core::app::Ui;
use xui_core::arrange::{
    button, column, combo_box, label, list, progress, row, status_bar, toolbar, Build, Handle,
    LayoutExt, Mounted,
};
use xui_core::backend::Result;
use xui_core::layout::Insets;
use xui_core::widget::{
    Button, ComboBox, Dialog, FileDialog, Fill, Label, ListView, Lucide, Menu, MenuId, ProgressBar,
    StatusBar, TaskDialog, TaskDialogIcon, Toolbar,
};
use xui_core::Dip;

use crate::app::Msg;
use crate::folder::Column;
use crate::host::Host;

const TOOLBAR_HEIGHT: Dip = Dip(36.0);
const ADDRESS_HEIGHT: Dip = Dip(26.0);
const PROGRESS_HEIGHT: Dip = Dip(30.0);

/// Context-menu commands.
pub const MENU_OPEN: MenuId = MenuId::new(0);
pub const MENU_EXTRACT: MenuId = MenuId::new(1);
pub const MENU_DELETE: MenuId = MenuId::new(2);
pub const MENU_TEST: MenuId = MenuId::new(3);

/// The toolbar's commands, in the order of its items.
const COMMANDS: [fn() -> Msg; 7] = [
    || Msg::Open,
    || Msg::New,
    || Msg::Add,
    || Msg::Extract,
    || Msg::Test,
    || Msg::Delete,
    || Msg::Up,
];

/// The widgets the app updates after building.
pub struct Widgets {
    pub list: Rc<ListView<Msg>>,
    pub address: Rc<Label<Msg>>,
    pub welcome: Rc<Label<Msg>>,
    pub level: Rc<ComboBox<Msg>>,
    pub progress: Rc<ProgressBar<Msg>>,
    pub progress_label: Rc<Label<Msg>>,
    pub cancel: Rc<Button<Msg>>,
    pub status: Rc<StatusBar<Msg>>,
    pub menu: Menu<Msg>,
    pub mounted: Mounted<Msg>,
}

/// The dialogs, built hidden.
pub struct Dialogs {
    pub open: FileDialog<Msg>,
    pub new: FileDialog<Msg>,
    pub add: FileDialog<Msg>,
    /// Built when needed: its field and message name the selection.
    pub extract: Option<Dialog<Msg>>,
    pub delete: Option<TaskDialog<Msg>>,
    pub message: Dialog<Msg>,
}

impl Dialogs {
    /// Whether any dialog is open (shortcuts are off while one is).
    pub fn any_open(&self) -> bool {
        self.open.is_open()
            || self.new.is_open()
            || self.add.is_open()
            || self.extract.as_ref().is_some_and(Dialog::is_open)
            || self.delete.as_ref().is_some_and(TaskDialog::is_open)
            || self.message.is_open()
    }
}

/// The command toolbar.
fn commands() -> Build<Toolbar<Msg>, Msg> {
    toolbar()
        .item_with_text(Lucide::FolderOpen, "Open an archive (Ctrl+O)", "Open")
        .item_with_text(Lucide::Package, "New archive (Ctrl+N)", "New")
        .item_with_text(Lucide::Plus, "Add a file to the archive", "Add")
        .item_with_text(
            Lucide::Download,
            "Extract the selection or everything (Ctrl+E)",
            "Extract",
        )
        .item_with_text(Lucide::Check, "Test the archive's checksums", "Test")
        .item_with_text(
            Lucide::Trash2,
            "Delete the selection from the archive (Del)",
            "Delete",
        )
        .separator()
        .item_with_text(Lucide::ChevronUp, "Up one folder (Backspace)", "Up")
        .then(|bar| bar.on_click(|index| COMMANDS.get(index).map(|msg| msg())))
}

/// The report list: one column per [`Column`], filled by the app.
fn report() -> Build<ListView<Msg>, Msg> {
    let mut list = list();
    for column in Column::ALL {
        list = match column {
            Column::Name => list.column(column.title(), Fill),
            Column::Size | Column::Packed => list.column_right(column.title(), Dip(90.0)),
            Column::Modified => list.column(column.title(), Dip(130.0)),
            Column::Method => list.column(column.title(), Dip(90.0)),
        };
    }
    list.then(|list| {
        list.multi_select(true)
            .on_selection(|rows| Some(Msg::Selection(rows.to_vec())))
            .on_activate(|row| Some(Msg::Activate(row)))
            .on_sort(|column| Some(Msg::Sort(column)))
            .on_context(|row, at| Some(Msg::Context(row, at)))
    })
}

/// Every extension the app reads, for the Open picker's filter.
fn readable_extensions() -> Vec<&'static str> {
    FORMATS
        .iter()
        .flat_map(|format| format.extensions().iter().copied())
        .collect()
}

/// Builds the dialogs, hidden.
fn dialogs(ui: &Ui<Msg>, host: &Host) -> Result<Dialogs> {
    let picker = |dialog: FileDialog<Msg>| {
        dialog
            .file_system(Rc::clone(&host.file_system))
            .initial_dir(host.start_dir.clone())
            .on_cancel(|| Some(Msg::PickerClosed))
    };
    let readable = readable_extensions();
    let open = picker(FileDialog::open_file(ui, "Open archive")?)
        .require_existing(true)
        .filter("Archives", &readable)
        .filter("All files", &[])
        .on_accept(|path| Some(Msg::OpenChosen(path)));
    let mut new = picker(FileDialog::save_file(ui, "New archive")?);
    for format in FORMATS.iter().filter(|f| f.writable()) {
        new = new.filter(
            &format!("{} (.{})", format.name(), format.extensions()[0]),
            format.extensions(),
        );
    }
    let new = new
        .suggested_name("archive.zip")
        .on_accept(|path| Some(Msg::NewChosen(path)));
    let add = picker(FileDialog::open_file(ui, "Add a file")?)
        .require_existing(true)
        .filter("All files", &[])
        .on_accept(|path| Some(Msg::AddChosen(path)));
    let message = Dialog::message(ui, "Archiver", "")?.on_action(|_| Some(Msg::MessageClosed));
    Ok(Dialogs {
        open,
        new,
        add,
        extract: None,
        delete: None,
        message,
    })
}

/// Builds the window's widgets and mounts the layout.
pub fn build(ui: &Ui<Msg>, host: &Host) -> Result<(Widgets, Dialogs)> {
    let list = Handle::new();
    let address = Handle::new();
    let welcome = Handle::new();
    let level = Handle::new();
    let progress_bar = Handle::new();
    let progress_label = Handle::new();
    let cancel = Handle::new();
    let status = Handle::new();
    let menu = Menu::context(ui)
        .build(|scope| {
            scope.item(MENU_OPEN, "Open");
            scope.item(MENU_EXTRACT, "Extract...");
            scope.item(MENU_DELETE, "Delete");
            scope.separator();
            scope.item(MENU_TEST, "Test archive");
        })
        .on_select(|id| Some(Msg::Menu(id)));
    let dialogs = dialogs(ui, host)?;

    let levels: Vec<&str> = Level::ALL.iter().map(|level| level.label()).collect();
    let normal = Level::ALL
        .iter()
        .position(|l| *l == Level::Normal)
        .unwrap_or(0);
    let gap = 6;
    let root = column().children((
        row()
            .gap(gap)
            .children((
                commands().fill(1),
                row()
                    .gap(gap)
                    .padding(Insets::symmetric(Dip(6.0), Dip(5.0)))
                    .children((
                        label("Level"),
                        combo_box(&levels)
                            .bind(&level)
                            .on_select(Msg::Level)
                            .then(move |level| {
                                level.select(normal);
                                level
                            })
                            .width(104),
                    ))
                    .fixed(166),
            ))
            .fixed(TOOLBAR_HEIGHT),
        row()
            .padding(Insets::symmetric(Dip(8.0), Dip(3.0)))
            .child(label("").bind(&address).fill(1))
            .fixed(ADDRESS_HEIGHT),
        report().bind(&list).fill(1),
        row()
            .padding(Insets::symmetric(Dip(16.0), Dip(0.0)))
            .child(
                label("Drop an archive here to open it, or files and folders to make a new one.")
                    .bind(&welcome)
                    .fill(1),
            )
            .fill(1),
        row()
            .gap(gap)
            .padding(Insets::symmetric(Dip(8.0), Dip(3.0)))
            .children((
                label("").bind(&progress_label).width(300),
                progress(1000).bind(&progress_bar).fill(1),
                button("Cancel")
                    .bind(&cancel)
                    .on_click_with(|| Some(Msg::Cancel))
                    .width(84),
            ))
            .fixed(PROGRESS_HEIGHT),
        status_bar(&["Ready", "", ""]).bind(&status),
    ));
    let mounted = ui.mount(root)?;
    let widgets = Widgets {
        list: list.get(),
        address: address.get(),
        welcome: welcome.get(),
        level: level.get(),
        progress: progress_bar.get(),
        progress_label: progress_label.get(),
        cancel: cancel.get(),
        status: status.get(),
        menu,
        mounted,
    };
    Ok((widgets, dialogs))
}

/// The Extract prompt, its field holding `dest`.
pub fn extract_dialog(ui: &Ui<Msg>, what: &str, dest: &str) -> Result<Dialog<Msg>> {
    Ok(Dialog::prompt(
        ui,
        "Extract",
        &format!("Extract {what} to this folder (created if missing):"),
        dest,
    )?
    .accept_label("Extract")
    .on_action(|action| Some(Msg::ExtractTo(action))))
}

/// The delete confirmation for `what`.
pub fn delete_dialog(ui: &Ui<Msg>, what: &str) -> Result<TaskDialog<Msg>> {
    TaskDialog::new(
        ui,
        "Delete from the archive?",
        &format!("{what} will be removed from the archive. This cannot be undone."),
    )?
    .icon(TaskDialogIcon::Warning)
    .command("Delete")
    .map(|dialog| dialog.on_action(|action| Some(Msg::DeleteConfirmed(action))))
}

/// The New picker's suggested name for `sources`: the first one's name with
/// the format's extension.
pub fn suggested_name(sources: &[std::path::PathBuf], format: Format) -> String {
    let stem = sources
        .first()
        .and_then(|path| path.file_stem())
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "archive".to_owned());
    format!("{stem}.{}", format.extensions()[0])
}
