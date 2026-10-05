//! The desktop's icons as an icon-view model, what activating one does, and
//! the desktop's side of drag and drop.
//!
//! The icons sit in right-anchored columns ([`lazyshell::desktop::grid`]):
//! the icon view's slots map to items through the grid, and a slot in a
//! short column's foot is empty. Activating an icon launches a shortcut's
//! app, browses a folder (or a shortcut to one) in Files, or hands a file to
//! `mimed`. An icon of the desktop folder can be dragged out as a
//! `text/uri-list` (onto a Files window, say), and one dropped on the
//! desktop goes into the folder ([`Ctx::drop_on_desktop`]).

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use lazyshell::desktop::folder::{Item, Kind};
use lazyshell::desktop::grid::Grid;
use lazyshell::Rect as ShellRect;
use xui_core::backend::{Canvas, WidgetId, WindowId};
use xui_core::icon::IconRef;
use xui_core::image::Image;
use xui_core::widget::IconModel;
use xui_core::{Rect, Theme};
use xui_explorer::platform::Launcher;
use xui_icons::{Icon, Palette, Tone};

use super::ctx::Ctx;
use super::deskdir::{open_action, Open};
use crate::backend::{DragOffer, DropEvent};
use crate::platform::launcher::LazyLauncher;
use crate::platform::urilist;

/// The app that browses folders.
const FILES_APP: &str = "os.lazy.files";

/// The icons as an icon model.
pub struct Icons {
    pub items: Vec<Item>,
    /// Each item's package icon, when it has one.
    pub images: Vec<Option<Rc<Image>>>,
    pub dark: bool,
    pub grid: Grid,
}

impl Icons {
    fn at(&self, slot: usize) -> Option<(usize, &Item)> {
        let index = self.grid.item_at(slot)?;
        self.items.get(index).map(|item| (index, item))
    }
}

impl IconModel for Icons {
    fn items(&self) -> usize {
        self.grid.slots()
    }

    fn icon(&self, _item: usize) -> Option<IconRef> {
        None
    }

    fn paint_icon(&self, slot: usize, canvas: &mut dyn Canvas, rect: Rect, _: &Theme, _: u32) -> bool {
        let Some((index, item)) = self.at(slot) else {
            return true;
        };
        if let Some(image) = self.images.get(index).and_then(Option::as_ref) {
            canvas.draw_image(image, rect);
            return true;
        }
        /// The set's near-black ink is invisible on a dark wallpaper.
        const DARK: Palette =
            Palette::GLOBAL_VILLAGE.with(Tone::Ink, xui_core::backend::Rgba::rgb(0xEC, 0xE6, 0xFF));
        let palette = if self.dark {
            &DARK
        } else {
            &Palette::GLOBAL_VILLAGE
        };
        xui_icons::draw(canvas, picture(item), rect, palette);
        true
    }

    fn line(&self, slot: usize, line: usize) -> Option<&str> {
        let (_, item) = self.at(slot)?;
        (line == 0).then_some(item.label.as_str())
    }
}

/// The picture an icon shows when it has no package icon.
fn picture(item: &Item) -> Icon {
    match &item.kind {
        Kind::App(app) => app_picture(app),
        Kind::Folder => Icon::Folder,
        Kind::Link(path) if !path.rsplit('/').next().unwrap_or("").contains('.') => Icon::Folder,
        Kind::Link(path) => file_picture(path),
        Kind::File => file_picture(&item.name),
    }
}

/// The picture of an app with no package icon (the built-ins, or an `init`
/// that did not answer): the core apps (by `system_name`, or a pre-F5 short
/// id) get a matching picture, any other app the generic window. The core
/// packages' own icons are drawn from the same pictures (`crates/app-icons`).
pub fn app_picture(app: &str) -> Icon {
    match app.strip_prefix("os.lazy.").unwrap_or(app) {
        "files" => Icon::Folder,
        "terminal" => Icon::Terminal,
        "editor" | "writer" => Icon::Document,
        "docs" => Icon::Help,
        "settings" => Icon::Settings,
        "confd" => Icon::Server,
        "sysmon" => Icon::Monitor,
        "paint" => Icon::Image,
        "fabricmon" => Icon::PubSub,
        "widget" | "counter" => Icon::Widget,
        "installer" | "archiver" => Icon::Archive,
        _ => Icon::Window,
    }
}

/// A file's picture by its extension.
fn file_picture(name: &str) -> Icon {
    let ext = name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase());
    match ext.as_deref() {
        Some("png" | "jpg" | "jpeg" | "gif" | "bmp") => Icon::Image,
        Some("zip" | "tar" | "gz" | "tgz" | "zst" | "xz" | "7z" | "lzp") => Icon::Archive,
        _ => Icon::Document,
    }
}

/// Activate the icon in view slot `slot`, zooming a launched window from
/// `origin` (design pixels).
pub fn activate(ctx: &Ctx, slot: usize, grid: Grid, origin: Option<ShellRect>) {
    let Some(item) = grid
        .item_at(slot)
        .and_then(|index| ctx.icons.borrow().get(index).cloned())
    else {
        return;
    };
    let path = ctx.icon_path(&item);
    match open_action(&item, path) {
        Some(Open::Launch(app)) => {
            let _ = ctx.launch(&app, origin);
        }
        Some(Open::Browse(dir)) => match dir.to_str() {
            Some(dir) => {
                let _ = ctx.launch_with(FILES_APP, dir, origin);
            }
            None => println!("SHELL:OPEN:FAIL not UTF-8"),
        },
        Some(Open::File(file)) => match LazyLauncher::new().open(&file) {
            Ok(()) => println!("SHELL:OPEN:PASS {}", file.display()),
            Err(error) => println!("SHELL:OPEN:FAIL {}: {error}", file.display()),
        },
        None => {}
    }
}

/// What a drag out of the desktop may carry: the icon view and the paths of
/// the selected icons, kept current by the desktop's `update`.
#[derive(Default)]
pub struct DragSource {
    pub view: Option<WidgetId>,
    pub paths: Vec<PathBuf>,
}

/// Wire the desktop's drag and drop: a press-and-drag on a selected folder
/// icon offers it, and a `text/uri-list` dropped on the desktop window
/// `desktop` goes into the folder. The hooks hold the context weakly (the
/// backend that owns them belongs to it).
pub fn wire_drag(ctx: &Rc<Ctx>, desktop: WindowId, source: Rc<RefCell<DragSource>>) {
    ctx.backend.on_drag_gesture(move |_, widget, _| {
        let source = source.borrow();
        if source.view != Some(widget) || source.paths.is_empty() {
            return None;
        }
        println!("SHELL:DESKTOP:DRAG:{}", source.paths.len());
        Some(DragOffer {
            mime: urilist::MIME.to_owned(),
            bytes: urilist::encode(&source.paths).into_bytes(),
        })
    });
    let weak: Weak<Ctx> = Rc::downgrade(ctx);
    ctx.backend.on_drag_event(move |window, event| {
        let DropEvent::Drop { mime, data, .. } = event else {
            return;
        };
        let Some(ctx) = weak.upgrade() else {
            return;
        };
        if window != desktop || mime != urilist::MIME {
            return;
        }
        match data {
            Ok(bytes) => ctx.drop_on_desktop(&urilist::decode(bytes)),
            Err(code) => println!("SHELL:DESKTOP:DROP:FAIL paste err={code}"),
        }
    });
}
