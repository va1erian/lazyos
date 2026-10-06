//! The window's widgets: the command toolbar, the page view filling the
//! rest, and the status bar (file, page, zoom). Dialogs are built once,
//! hidden, and kept for the window's life.

use std::cell::RefCell;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{build, column, status_bar, toolbar, Build, Handle, LayoutExt, Mounted};
use xui_core::backend::Result;
use xui_core::widget::{Dialog, FileDialog, Lucide, StatusBar, Toolbar};
use xui_core::Dip;

use crate::app::Msg;
use crate::host::Host;
use crate::view::PageView;
use crate::viewer::Viewer;

const TOOLBAR_HEIGHT: Dip = Dip(36.0);

/// The toolbar's commands, in the order of its items.
const COMMANDS: [fn() -> Msg; 7] = [
    || Msg::Open,
    || Msg::PreviousPage,
    || Msg::NextPage,
    || Msg::ZoomStep(false),
    || Msg::ZoomStep(true),
    || Msg::FitWidth,
    || Msg::FitPage,
];

pub struct Widgets {
    pub view: Rc<PageView>,
    pub status: Rc<StatusBar<Msg>>,
    /// Keeps the layout mounted for the window's life.
    pub _mounted: Mounted<Msg>,
}

pub struct Dialogs {
    pub open: FileDialog<Msg>,
    pub password: Dialog<Msg>,
    pub message: Dialog<Msg>,
}

impl Dialogs {
    /// Whether any dialog is open (the window's shortcuts are off then).
    pub fn any_open(&self) -> bool {
        self.open.is_open() || self.password.is_open() || self.message.is_open()
    }
}

fn commands() -> Build<Toolbar<Msg>, Msg> {
    toolbar()
        .item_with_text(Lucide::FolderOpen, "Open a document (Ctrl+O)", "Open")
        .separator()
        .item(Lucide::ChevronUp, "Previous page (P, Ctrl+Page Up)")
        .item(Lucide::ChevronDown, "Next page (N, Ctrl+Page Down)")
        .separator()
        .item(Lucide::Minus, "Zoom out (Ctrl+-)")
        .item(Lucide::Plus, "Zoom in (Ctrl+=)")
        .item_with_text(
            Lucide::RectangleHorizontal,
            "Fit the page width (Ctrl+1)",
            "Width",
        )
        .item_with_text(Lucide::BookOpen, "Fit the whole page (Ctrl+2)", "Page")
        .then(|bar| bar.on_click(|index| COMMANDS.get(index).map(|msg| msg())))
}

fn dialogs(ui: &Ui<Msg>, host: &Host) -> Result<Dialogs> {
    let open = FileDialog::open_file(ui, "Open document")?
        .file_system(Rc::clone(&host.file_system))
        .initial_dir(host.start_dir.clone())
        .require_existing(true)
        .filter("PDF documents", &["pdf"])
        .filter("All files", &[])
        .on_cancel(|| Some(Msg::DialogClosed))
        .on_accept(|path| Some(Msg::OpenChosen(path)));
    // xui gap: `Dialog::prompt` has no masked mode, so the password shows
    // as it is typed (docs/pdf-reader-plan.md, risks).
    let password = Dialog::prompt(ui, "Password", "", "")?
        .accept_label("Open")
        .on_action(|action| Some(Msg::Password(action)));
    let message = Dialog::message(ui, "PDF Viewer", "")?.on_action(|_| Some(Msg::DialogClosed));
    Ok(Dialogs {
        open,
        password,
        message,
    })
}

/// Builds the window over `viewer` and mounts it.
pub fn build_window(
    ui: &Ui<Msg>,
    host: &Host,
    viewer: Rc<RefCell<Viewer>>,
) -> Result<(Widgets, Dialogs)> {
    let (view, status) = (Handle::new(), Handle::new());
    let dialogs = dialogs(ui, host)?;
    let root = column().children((
        commands().fixed(TOOLBAR_HEIGHT),
        build(move |ui| PageView::new(ui, viewer))
            .bind(&view)
            .fill(1),
        status_bar(&["No document", "", ""]).bind(&status),
    ));
    let mounted = ui.mount(root)?;
    Ok((
        Widgets {
            view: view.get(),
            status: status.get(),
            _mounted: mounted,
        },
        dialogs,
    ))
}
