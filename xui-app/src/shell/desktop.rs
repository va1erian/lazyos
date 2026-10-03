//! The desktop surface: the wallpaper and an `IconView` of launchers, and the
//! shell's heartbeat.
//!
//! The desktop is the primary xui window, so its timer drives everything that
//! is not a pointer event on a panel: the shell subscription's events, the
//! `os.lazy.shell` requests, the clock, the theme and the launcher list.

use std::rc::Rc;

use lazyshell::taskbar::BAR_H;
use lazyshell::{Entry, Rect as ShellRect};
use xui_core::app::{App, Ui};
use xui_core::backend::{Canvas, PlatformSpec};
use xui_core::icon::IconRef;
use xui_core::image::Image;
use xui_core::widget::{IconModel, IconSize, IconView};
use xui_core::{Dip, Rect, Theme};
use xui_icons::{Icon, Palette, Tone};

use super::ctx::Ctx;
use super::icons::IconCache;
use super::taskbar::{self, BarApp};
use super::theme::desktop_theme;
use super::{heartbeat, link, menu, service};
use crate::client_window::SurfaceRole;

/// The heartbeat period. The backend's loop parks about a tick per window, so
/// this is roughly every pass.
const TICK_MILLIS: u32 = 30;
/// Inset of the launcher column from the screen's top-right corner. The column
/// sits on the right because the compositor places new windows from the
/// top-left, where they would cover the icons and their labels.
const ICONS_INSET: i32 = 12;
/// Width of the launcher column: one medium tile wide.
const ICONS_W: i32 = 210;
/// Side of the launch-origin square around the pointer.
const LAUNCH_TILE: i32 = 48;

/// A desktop message.
pub enum DeskMsg {
    Tick,
    /// Activate launcher `index` (double-click).
    Launch(usize),
}

/// The desktop window's app.
pub struct DesktopApp {
    ctx: Rc<Ctx>,
    icons: IconView<DeskMsg>,
    beat: heartbeat::Heartbeat,
    service: service::ShellService,
    /// The package icons decoded so far.
    images: IconCache,
}

/// The launchers as an icon model.
struct Launchers {
    entries: Vec<Entry>,
    /// Each launcher's package icon, when it has one.
    images: Vec<Option<Rc<Image>>>,
    dark: bool,
}

impl IconModel for Launchers {
    fn items(&self) -> usize {
        self.entries.len()
    }

    fn icon(&self, _item: usize) -> Option<IconRef> {
        None
    }

    fn paint_icon(
        &self,
        item: usize,
        canvas: &mut dyn Canvas,
        rect: Rect,
        _: &Theme,
        _: u32,
    ) -> bool {
        let Some(entry) = self.entries.get(item) else {
            return false;
        };
        if let Some(image) = self.images.get(item).and_then(Option::as_ref) {
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
        xui_icons::draw(canvas, icon_for(&entry.app), rect, palette);
        true
    }

    fn line(&self, item: usize, line: usize) -> Option<&str> {
        (line == 0).then(|| self.entries.get(item).map(|e| e.label.as_str()))?
    }
}

/// The picture a launcher shows when its app has no package icon (the
/// built-ins, or an `init` that did not answer): the core apps (by
/// `system_name`, or the short id a launcher saved before F5 holds) get a
/// matching picture, any other app the generic window. The core packages'
/// own icons are drawn from the same pictures (`crates/app-icons`).
fn icon_for(app: &str) -> Icon {
    match app.strip_prefix("os.lazy.").unwrap_or(app) {
        "files" => Icon::Folder,
        "terminal" => Icon::Terminal,
        "editor" => Icon::Document,
        "docs" => Icon::Help,
        "settings" => Icon::Settings,
        "confd" => Icon::Server,
        "sysmon" => Icon::Monitor,
        "paint" => Icon::Image,
        "fabricmon" => Icon::PubSub,
        "widget" | "counter" => Icon::Widget,
        "installer" => Icon::Archive,
        _ => Icon::Window,
    }
}

/// The desktop window's spec: the whole screen.
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
        let bottom = ctx.screen.1 - BAR_H - ICONS_INSET;
        let right = ctx.screen.0 - ICONS_INSET;
        let area = Rect::new(right - ICONS_W, ICONS_INSET, right, bottom);
        let model = Launchers {
            entries: ctx.launchers.borrow().clone(),
            images: Vec::new(),
            dark,
        };
        let icons = IconView::with_model(ui, area, model)
            .expect("desktop icons")
            .on_activate(|index| Some(DeskMsg::Launch(index)));
        icons.set_icon_size(IconSize::Medium);
        icons.select(None);

        open_bar(&ctx, ui);
        let (w, h) = ctx.screen;
        if let Err(code) = ctx.client.set_work_area(0, 0, w, h - BAR_H) {
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
            beat: heartbeat::Heartbeat::new(),
            service: service::ShellService::default(),
            images: IconCache::default(),
        }
    }

    /// Launch launcher `index`, zooming its window open from the tile the
    /// pointer double-clicked (the desktop is at the screen origin, so window
    /// pixels are screen pixels).
    fn launch(&self, index: usize) {
        let Some(app) = self
            .ctx
            .launchers
            .borrow()
            .get(index)
            .map(|e| e.app.clone())
        else {
            return;
        };
        let (x, y) = self.ctx.backend.pointer();
        let (w, h) = self.ctx.screen;
        let origin = (x >= 0 && y >= 0 && x < w && y < h).then(|| {
            ShellRect::new(
                x - LAUNCH_TILE / 2,
                y - LAUNCH_TILE / 2,
                LAUNCH_TILE,
                LAUNCH_TILE,
            )
        });
        let _ = self.ctx.launch(&app, origin);
    }

    /// One heartbeat: events, requests, clock, theme, launchers.
    fn tick(&mut self, ui: &Ui<DeskMsg>) {
        if self.ctx.bar.borrow().is_none() && self.beat.retry_bar() {
            open_bar(&self.ctx, ui);
            self.ctx.bar_changed();
        }
        link::pump(&self.ctx, ui);
        self.service.pump(&self.ctx, ui, &mut self.beat);
        if self.beat.clock(&self.ctx) {
            self.ctx.repaint_bar();
        }
        if self.beat.theme(&self.ctx) {
            let dark = self.ctx.theme.borrow().is_dark();
            ui.set_theme(desktop_theme(&self.ctx.theme.borrow().palette(), dark));
            self.rebuild_icons();
            // A new clock format (Settings, Time & Date) changes the width
            // the clock reserves, so the entries move.
            if self.ctx.bar.borrow().is_some() && taskbar::measure_clock(&self.ctx, ui) {
                self.ctx.bar_changed();
            }
            self.ctx.repaint_bar();
            self.ctx.repaint_menu();
        }
        if self.beat.launchers_due() {
            self.ctx.reload_launchers();
        }
        if self.ctx.launchers_changed.replace(false) {
            self.rebuild_icons();
        }
        self.beat.report_first_frame(&self.ctx, self.icons.len());
    }

    fn rebuild_icons(&mut self) {
        let images = self
            .ctx
            .launcher_icons
            .borrow()
            .iter()
            .map(|path| self.images.get(path))
            .collect();
        self.icons.set_model(Launchers {
            entries: self.ctx.launchers.borrow().clone(),
            images,
            dark: self.ctx.theme.borrow().is_dark(),
        });
        self.icons.select(None);
    }
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
        }
    }
}

/// Create the taskbar panel at the bottom of the screen. A failure (an older
/// compositor without panels) is logged once and retried by the heartbeat.
fn open_bar<M: 'static>(ctx: &Rc<Ctx>, ui: &Ui<M>) {
    let (w, _) = ctx.screen;
    ctx.backend.set_next_role(SurfaceRole::Panel {
        x: 0,
        y: ctx.bar_y(),
    });
    let spec = PlatformSpec::new("LazyShell taskbar").size(Dip(w as f32), Dip(BAR_H as f32));
    let built = Rc::clone(ctx);
    match ui.open_window(spec, move |ui| BarApp::build(built, ui)) {
        Ok(handle) => *ctx.bar.borrow_mut() = Some(handle),
        Err(error) => ctx.note("bar-open", || format!("SHELL:TASKBAR:FAIL {error}")),
    }
}
