//! The window's widgets: the transport toolbar, the picture filling the
//! middle, the seek bar and the status bar (file, time, format). Dialogs are
//! built once, hidden, and kept for the window's life.

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{
    build, column, slider, status_bar, toolbar, Build, Handle, LayoutExt, Mounted,
};
use xui_core::backend::Result;
use xui_core::widget::{FileDialog, Lucide, Slider, StatusBar, Toolbar};
use xui_core::Dip;

use crate::app::Msg;
use crate::view::VideoView;
use xui_host::dialogs::open_dialog;
use xui_host::error::ErrorDialog;
use xui_host::AppHost;

const TOOLBAR_HEIGHT: Dip = Dip(36.0);
const SEEK_HEIGHT: Dip = Dip(28.0);

/// The seek bar's range: the position in thousandths of the duration.
pub const SEEK_MAX: f64 = 1000.0;

/// The toolbar's commands, in the order of its items.
const COMMANDS: [fn() -> Msg; 3] = [|| Msg::Open, || Msg::TogglePlay, || Msg::Stop];

pub struct Widgets {
    pub view: Rc<VideoView>,
    pub seek: Rc<Slider<Msg>>,
    pub status: Rc<StatusBar<Msg>>,
    /// Keeps the layout mounted for the window's life.
    pub _mounted: Mounted<Msg>,
}

pub struct Dialogs {
    pub open: FileDialog<Msg>,
    pub message: ErrorDialog<Msg>,
}

impl Dialogs {
    /// Whether any dialog is open (the window's shortcuts are off then).
    pub fn any_open(&self) -> bool {
        self.open.is_open() || self.message.is_open()
    }
}

fn commands() -> Build<Toolbar<Msg>, Msg> {
    toolbar()
        .item_with_text(Lucide::FolderOpen, "Open a video (Ctrl+O)", "Open")
        .item(Lucide::Play, "Play or pause (Space)")
        .item(Lucide::Square, "Stop and go back to the start (S)")
        .then(|bar| bar.on_click(|index| COMMANDS.get(index).map(|msg| msg())))
}

fn dialogs(ui: &Ui<Msg>, host: &dyn AppHost) -> Result<Dialogs> {
    let open = open_dialog(ui, host, "Open video", &[("Video files", &["avi"])], || {
        Some(Msg::DialogClosed)
    })?
    .on_accept(|path| Some(Msg::OpenChosen(path)));
    let message = ErrorDialog::new(ui, "Video Player", || Some(Msg::DialogClosed))?;
    Ok(Dialogs { open, message })
}

/// Builds the window and mounts it.
pub fn build_window(ui: &Ui<Msg>, host: &dyn AppHost) -> Result<(Widgets, Dialogs)> {
    let (view, seek, status) = (Handle::new(), Handle::new(), Handle::new());
    let dialogs = dialogs(ui, host)?;
    let root = column().children((
        commands().fixed(TOOLBAR_HEIGHT),
        build(VideoView::new).bind(&view).fill(1),
        slider(0.0, SEEK_MAX)
            .on_change(Msg::Scrub)
            .then(|bar| bar.on_commit(|value| Some(Msg::Seek(value))))
            .bind(&seek)
            .fixed(SEEK_HEIGHT),
        status_bar(&["No video", "", ""]).bind(&status),
    ));
    let mounted = ui.mount(root)?;
    Ok((
        Widgets {
            view: view.get(),
            seek: seek.get(),
            status: status.get(),
            _mounted: mounted,
        },
        dialogs,
    ))
}
