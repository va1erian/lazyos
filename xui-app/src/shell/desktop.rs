//! The desktop surface: the wallpaper and an `IconView` of the desktop
//! folder's icons ([`super::deskicons`], [`super::deskdir`]), and the shell's
//! heartbeat.
//!
//! The desktop is the primary xui window, so its timer drives everything that
//! is not a pointer event on a panel: the shell subscription's events, the
//! `os.lazy.shell` requests, the clock, the theme and the desktop folder.

use std::cell::RefCell;
use std::rc::Rc;

use lazyshell::desktop::grid::Grid;
use lazyshell::taskbar::BAR_H;
use lazyshell::Rect as ShellRect;
use xui_core::app::{App, Ui};
use xui_core::arrange::{absolute, build, icon_view_with, Handle, LayoutExt, Mounted};
use xui_core::backend::PlatformSpec;
use xui_core::widget::{IconSize, IconView};
use xui_core::{Dip, Rect};

use super::ctx::Ctx;
use super::deskicons::{self, DragSource, Icons};
use super::icons::IconCache;
use super::taskbar::{self, BarApp};
use super::theme::desktop_theme;
use super::wallpaper::Wallpaper;
use super::{heartbeat, link, menu, notice, service};
use crate::client_window::SurfaceRole;

/// The heartbeat period. The backend's loop parks about a tick per window, so
/// this is roughly every pass.
const TICK_MILLIS: u32 = 30;
/// Inset of the icon columns from the screen's top-left corner.
const ICONS_INSET: i32 = 12;
/// A medium icon tile's size and the gap between tiles (xui's metrics), and
/// the slack a column keeps beside its tile.
const TILE_W: i32 = 190;
const TILE_H: i32 = 50;
const TILE_GAP: i32 = 4;
const COLUMN_SLACK: i32 = 20;
/// Side of the launch-origin square around the pointer.
const LAUNCH_TILE: i32 = 48;

/// A desktop message.
pub enum DeskMsg {
    Tick,
    /// Activate the icon in view slot `index` (double-click).
    Launch(usize),
    /// The icon selection changed.
    Selection,
}

/// The desktop window's app.
pub struct DesktopApp {
    ctx: Rc<Ctx>,
    icons: Rc<IconView<DeskMsg>>,
    /// The layout that places the icon view at its columns.
    placement: Mounted<DeskMsg>,
    beat: heartbeat::Heartbeat,
    service: service::ShellService,
    /// The package icons decoded so far.
    images: IconCache,
    wallpaper: Wallpaper,
    /// The icon columns in screen pixels: where the labels sit.
    labels: ShellRect,
    /// Whether the icons sit on something dark (the picture, else the mode).
    dark: bool,
    /// How the view's slots map to icons.
    grid: Grid,
    /// What a drag out of the desktop carries.
    drag: Rc<RefCell<DragSource>>,
}

/// The desktop window's spec: the whole screen (in design pixels, like
/// every shell size; the backend draws it at the UI scale).
pub fn spec(ctx: &Ctx) -> PlatformSpec {
    let (w, h) = ctx.screen;
    PlatformSpec::new("LazyShell desktop").size(Dip(w as f32), Dip(h as f32))
}

impl DesktopApp {
    /// Build the desktop, then the taskbar panel; take the work area and
    /// start the heartbeat. Called once the shell subscription is held.
    pub fn build(ctx: Rc<Ctx>, windows: usize, ui: &mut Ui<DeskMsg>) -> DesktopApp {
        let dark = ctx.theme.borrow().is_dark();
        ui.set_theme(desktop_theme(&ctx.theme.borrow().palette(), dark));
        let grid = Grid::new(ctx.icons.borrow().len(), rows_fit(&ctx));
        let area = icons_area(&ctx, grid);
        let model = Icons {
            items: ctx.icons.borrow().clone(),
            images: Vec::new(),
            dark,
            grid,
        };
        let handle = Handle::new();
        let (x, y, w, h) = icons_design(&ctx, grid);
        let placement = ui
            .mount(
                absolute().child(
                    icon_view_with(model)
                        .then(|view| {
                            view.on_activate(|index| Some(DeskMsg::Launch(index)))
                                .on_selection(|_| Some(DeskMsg::Selection))
                        })
                        .bind(&handle)
                        .at(x, y, w, h),
                ),
            )
            .expect("desktop icons");
        let icons = handle.get();
        icons.set_icon_size(IconSize::Medium);
        icons.select(None);
        let drag = Rc::new(RefCell::new(DragSource {
            view: Some(icons.id()),
            paths: Vec::new(),
        }));
        deskicons::wire_drag(&ctx, ui.window(), Rc::clone(&drag));

        open_bar(&ctx, ui);
        let (w, h) = ctx.screen;
        let work = ctx.to_screen(ShellRect::new(0, 0, w, h - BAR_H));
        if let Err(code) = ctx.client.set_work_area(work.x, work.y, work.w, work.h) {
            ctx.note("workarea", || format!("SHELL:WORKAREA:FAIL err={}", -code));
        }
        ctx.bar_changed();
        println!("SHELL:UP:PASS");
        if windows > 0 {
            println!("SHELL:RESTART:PASS windows={windows}");
        }
        ui.on_timer(|_| Some(DeskMsg::Tick));
        ui.set_timer(TICK_MILLIS);
        DesktopApp {
            ctx,
            icons,
            placement,
            beat: heartbeat::Heartbeat::new(),
            service: service::ShellService::default(),
            images: IconCache::default(),
            wallpaper: Wallpaper::default(),
            labels: ShellRect::new(area.left, area.top, area.width(), area.height()),
            dark,
            grid,
            drag,
        }
    }

    /// Activate the icon in slot `index`, zooming a launched window open
    /// from the tile the pointer double-clicked (the desktop is at the
    /// screen origin, so window pixels are screen pixels, in design pixels
    /// here).
    fn launch(&self, index: usize) {
        let (x, y) = self.ctx.backend.pointer();
        let (x, y) = self.ctx.to_design(x, y);
        let (w, h) = self.ctx.screen;
        let origin = (x >= 0 && y >= 0 && x < w && y < h).then(|| {
            ShellRect::new(
                x - LAUNCH_TILE / 2,
                y - LAUNCH_TILE / 2,
                LAUNCH_TILE,
                LAUNCH_TILE,
            )
        });
        deskicons::activate(&self.ctx, index, self.grid, origin);
    }

    /// The selection changed: what a drag would now carry.
    fn selection_changed(&self) {
        let icons = self.ctx.icons.borrow();
        let paths = self
            .icons
            .selection()
            .into_iter()
            .filter_map(|slot| self.grid.item_at(slot))
            .filter_map(|index| icons.get(index))
            .filter_map(|item| self.ctx.icon_path(item))
            .collect();
        self.drag.borrow_mut().paths = paths;
    }

    /// One heartbeat: events, requests, clock, theme, desktop folder.
    fn tick(&mut self, ui: &Ui<DeskMsg>) {
        if self.ctx.bar.borrow().is_none() && self.beat.retry_bar() {
            open_bar(&self.ctx, ui);
            self.ctx.bar_changed();
        }
        link::pump(&self.ctx, ui);
        // `init` gave up on an app of this session: tell the user (#549).
        let failures = self.ctx.failures.borrow_mut().poll();
        for failure in failures {
            notice::post(&self.ctx, ui, failure);
        }
        notice::pump(&self.ctx, ui);
        self.service.pump(&self.ctx, ui, &mut self.beat);
        super::tray::pump(&self.ctx, ui);
        if self.beat.clock(&self.ctx) {
            self.ctx.repaint_bar();
        }
        if self.beat.theme(&self.ctx) {
            self.retheme(ui);
            self.rebuild_icons(ui);
            // A new clock format (Settings, Time & Date) changes the width
            // the clock reserves, so the entries move.
            if self.ctx.bar.borrow().is_some() && taskbar::measure_clock(&self.ctx, ui) {
                self.ctx.bar_changed();
            }
            self.ctx.repaint_bar();
            self.ctx.repaint_menu();
        }
        let apps_due = self.beat.launchers_due();
        if apps_due || self.beat.folder_due() {
            self.ctx.reload_desktop(apps_due);
        }
        if self.ctx.icons_changed.replace(false) {
            self.rebuild_icons(ui);
        }
        self.beat.report_first_frame(&self.ctx, self.icons.len());
    }

    /// Apply the settings just read: the picture (loaded when its path
    /// changed), then the colours, with label ink that reads on the picture.
    fn retheme(&mut self, ui: &Ui<DeskMsg>) {
        let feed = self.ctx.theme.borrow();
        let s = self.ctx.scale();
        let screen = (self.ctx.screen.0 * s, self.ctx.screen.1 * s);
        if self.wallpaper.sync(feed.wallpaper(), screen, self.labels) {
            self.ctx
                .backend
                .set_backdrop(ui.window(), self.wallpaper.image());
        }
        self.dark = self.wallpaper.dark().unwrap_or(feed.is_dark());
        ui.set_theme(desktop_theme(&feed.palette(), self.dark));
    }

    /// Rebuild the view from the icons: re-lay the columns when their count
    /// changed, then hand the view the new model.
    fn rebuild_icons(&mut self, ui: &Ui<DeskMsg>) {
        let images = self
            .ctx
            .icon_images
            .borrow()
            .iter()
            .map(|path| self.images.get_scaled(path, self.ctx.scale()))
            .collect();
        let grid = Grid::new(self.ctx.icons.borrow().len(), rows_fit(&self.ctx));
        if grid.columns != self.grid.columns {
            let area = icons_area(&self.ctx, grid);
            // Re-place the same view at the new columns: a fresh layout
            // around it, not a hand move a later relayout would undo.
            let view = Rc::clone(&self.icons);
            let (x, y, w, h) = icons_design(&self.ctx, grid);
            self.placement = ui
                .mount(absolute().child(build(move |_| Ok(view)).at(x, y, w, h)))
                .expect("desktop icons");
            self.labels = ShellRect::new(area.left, area.top, area.width(), area.height());
        }
        self.grid = grid;
        self.icons.set_model(Icons {
            items: self.ctx.icons.borrow().clone(),
            images,
            dark: self.dark,
            grid,
        });
        self.icons.select(None);
        self.drag.borrow_mut().paths.clear();
    }
}

/// How many icons one column holds on this screen.
fn rows_fit(ctx: &Ctx) -> usize {
    let height = ctx.screen.1 - BAR_H - ICONS_INSET * 2;
    usize::try_from((height + TILE_GAP) / (TILE_H + TILE_GAP)).unwrap_or(1)
}

/// The icon view's rectangle in design pixels for `grid`, as `(x, y, width,
/// height)`: as many columns as it has, anchored to the top-left corner.
fn icons_design(ctx: &Ctx, grid: Grid) -> (i32, i32, i32, i32) {
    let columns = i32::try_from(grid.columns).unwrap_or(1);
    let width = columns * (TILE_W + TILE_GAP) - TILE_GAP + COLUMN_SLACK;
    let bottom = ctx.screen.1 - BAR_H - ICONS_INSET;
    let right = (ICONS_INSET + width).min(ctx.screen.0);
    (ICONS_INSET, ICONS_INSET, right - ICONS_INSET, bottom - ICONS_INSET)
}

/// The same rectangle in screen pixels: where the labels sit.
fn icons_area(ctx: &Ctx, grid: Grid) -> Rect {
    let (x, y, w, h) = icons_design(ctx, grid);
    let s = ctx.scale();
    Rect::new(x * s, y * s, (x + w) * s, (y + h) * s)
}

impl App for DesktopApp {
    type Msg = DeskMsg;

    fn update(&mut self, msg: DeskMsg, ui: &mut Ui<DeskMsg>) {
        match msg {
            DeskMsg::Tick => self.tick(ui),
            DeskMsg::Launch(index) => {
                menu::close(&self.ctx);
                self.launch(index);
            }
            DeskMsg::Selection => self.selection_changed(),
        }
    }
}

/// Create the taskbar panel at the bottom of the screen. A failure (an older
/// compositor without panels) is logged once and retried by the heartbeat.
fn open_bar<M: 'static>(ctx: &Rc<Ctx>, ui: &Ui<M>) {
    let (w, _) = ctx.screen;
    ctx.backend.set_next_role(SurfaceRole::Panel {
        x: 0,
        y: ctx.bar_y() * ctx.scale(),
    });
    let spec = PlatformSpec::new("LazyShell taskbar").size(Dip(w as f32), Dip(BAR_H as f32));
    let built = Rc::clone(ctx);
    match ui.open_window(spec, move |ui| BarApp::build(built, ui)) {
        Ok(handle) => {
            *ctx.bar.borrow_mut() = Some(handle);
            super::probe::start_button(ctx);
        }
        Err(error) => ctx.note("bar-open", || format!("SHELL:TASKBAR:FAIL {error}")),
    }
}
