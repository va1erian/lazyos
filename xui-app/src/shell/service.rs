//! `os.lazy.shell` (`os.lazy.shell.v1`, idl/shell.midl): the shell's own
//! Messenger service, so scripts, tests and session programs can drive the
//! desktop the way a user would.
//!
//! Every call is checked against the sender's kernel-stamped uid (0 or the
//! shell's own; see `lazyshell::policy`): the uid the kernel stamped on the
//! message when it was queued (`Request::origin`), so the shell needs no
//! capability to identify callers. When its own uid is unknown it refuses
//! every call with `EACCES` (fail closed).

use crate::trace::{self, Channel};
use std::rc::Rc;

use libmessenger::server::{Caller, Fault};
use messenger_generated::os_lazy_shell_v1 as wire;
use xui_core::app::Ui;

use super::ctx::Ctx;
use super::heartbeat::Heartbeat;
use super::menu;
use crate::server::Server;
use crate::sys::errno;

/// The registered service name.
pub const NAME: &str = wire::SERVICE_NAME;
/// Most requests answered per heartbeat.
const REQUESTS_PER_TICK: usize = 8;
/// Receive buffer for one request (the largest is a short app id).
const REQUEST_BYTES: usize = 4096;

/// The service endpoint, once registered.
#[derive(Default)]
pub struct ShellService {
    server: Option<Server>,
}

impl ShellService {
    /// Register when due (retrying while a previous shell's name is still
    /// held), then answer what is queued.
    pub fn pump<M: 'static>(&mut self, ctx: &Rc<Ctx>, ui: &Ui<M>, beat: &mut Heartbeat) {
        if self.server.is_none() && beat.register_due() {
            match Server::register(NAME, &[wire::INTERFACE_ID], &[wire::INTERFACE_NAME]) {
                Ok(server) => {
                    self.server = Some(server);
                    trace::pass!(Channel::Shell, "SERVICE");
                }
                Err(code) => ctx.note("register", || format!("SHELL:SERVICE:RETRY err={}", -code)),
            }
        }
        let Some(server) = &self.server else {
            return;
        };
        let mut buf = vec![0u8; REQUEST_BYTES];
        for _ in 0..REQUESTS_PER_TICK {
            let request = match server.poll(&mut buf) {
                Ok(Some(request)) => request,
                Ok(None) => return,
                Err(code) => {
                    ctx.note("serve", || format!("SHELL:SERVICE:RECV:FAIL err={}", -code));
                    return;
                }
            };
            let reply = wire::dispatch(&mut Handler { ctx, ui }, &request);
            if let Some(txn) = request.txn {
                let _ = server.reply(txn, &reply);
            }
        }
    }
}

/// The shell's state, answering `os.lazy.shell.v1` (the generated server).
struct Handler<'a, M: 'static> {
    ctx: &'a Rc<Ctx>,
    ui: &'a Ui<M>,
}

/// A failed call: positive errno and friendly text.
type Failure = (i64, &'static str);

fn fault((code, text): Failure) -> Fault {
    Fault::new(code as u32, text)
}

impl<M: 'static> Handler<'_, M> {
    /// Refuse a caller that is neither uid 0 nor the shell's own user (the
    /// kernel-stamped uid); fail closed when the shell's own uid is unknown.
    fn allow(&self, cx: &Caller) -> Result<(), Fault> {
        let caller = cx.uid;
        if lazyshell::policy::caller_allowed(Some(caller), self.ctx.uid) {
            return Ok(());
        }
        self.ctx
            .note("deny", || format!("SHELL:SERVICE:DENY uid={caller}"));
        Err(fault((errno::EACCES, "caller not allowed")))
    }
}

impl<M: 'static> wire::Server for Handler<'_, M> {
    fn status(&mut self, cx: &Caller) -> Result<wire::StatusReply, Fault> {
        self.allow(cx)?;
        Ok(status(self.ctx))
    }

    fn show_start_menu(&mut self, cx: &Caller, open: bool) -> Result<(), Fault> {
        self.allow(cx)?;
        if open {
            menu::open(self.ctx, self.ui);
        } else {
            menu::close(self.ctx);
        }
        Ok(())
    }

    fn launch(&mut self, cx: &Caller, app: String) -> Result<u64, Fault> {
        self.allow(cx)?;
        if !deskmenu::valid_app_id(&app) {
            return Err(fault((errno::EINVAL, "not an app id")));
        }
        let pid = self
            .ctx
            .launch(&app, launch_origin(self.ctx, &app))
            .map_err(|code| fault((code.abs(), "launch failed")))?;
        Ok(pid)
    }

    fn refresh(&mut self, cx: &Caller) -> Result<wire::RefreshReply, Fault> {
        self.allow(cx)?;
        let ctx = self.ctx;
        ctx.reload_menu();
        ctx.reload_desktop(true);
        ctx.repaint_menu();
        Ok(wire::RefreshReply {
            menu: ctx.menu.borrow().rows().len() as u32,
            desktop: ctx.icons.borrow().len() as u32,
        })
    }

    fn activate(&mut self, cx: &Caller, surface: u64) -> Result<(), Fault> {
        self.allow(cx)?;
        if !self.ctx.taskbar.borrow().contains(surface) {
            return Err(fault((errno::ENOENT, "no such taskbar entry")));
        }
        self.ctx
            .client
            .activate_surface(surface)
            .map_err(|code| fault((code.abs(), "activate failed")))
    }
}

/// Where a scripted launch zooms from, as if the user had clicked it: the
/// app's start-menu row (where it would be, open or not), else the "LazyOS"
/// button.
fn launch_origin(ctx: &Ctx, app: &str) -> Option<lazyshell::Rect> {
    let menu = ctx.menu.borrow();
    if let Some(rect) = menu.find(app).and_then(|index| menu.row_rect(index)) {
        let (x, y) = menu.origin(ctx.screen.1);
        return Some(rect.offset(x, y));
    }
    Some(lazyshell::taskbar::START_BUTTON.offset(0, ctx.bar_y()))
}

/// The tray's items in bar order, then the overflow's.
fn tray_entries(ctx: &Ctx) -> Vec<wire::TrayEntry> {
    let model = ctx.tray.model.borrow();
    let layout = ctx.tray.layout.borrow();
    layout
        .cells
        .iter()
        .map(|cell| &cell.app)
        .chain(layout.overflow.iter())
        .filter_map(|app| model.get(app))
        .map(|entry| wire::TrayEntry {
            app: entry.app.clone(),
            tooltip: entry.tooltip().to_owned(),
            status: entry.status() as u32,
            custom: entry.custom.is_some(),
            visible: layout.visible(&entry.app),
        })
        .collect()
}

/// What the shell shows right now.
fn status(ctx: &Ctx) -> wire::StatusReply {
    let bar = ctx.taskbar.borrow();
    // A desktop icon reports the app its shortcut launches, else its path.
    let launcher = |item: &lazyshell::desktop::folder::Item| wire::Launcher {
        app: item
            .app()
            .map(str::to_owned)
            .or_else(|| ctx.icon_path(item).map(|p| p.display().to_string()))
            .unwrap_or_default(),
        label: item.label.clone(),
    };
    wire::StatusReply {
        windows: bar
            .windows()
            .iter()
            .map(|window| wire::TaskbarEntry {
                surface: window.surface,
                title: window.title.clone(),
                minimized: window.minimized,
            })
            .collect(),
        focused: bar.focused(),
        menu_open: ctx.menu_window.borrow().is_some(),
        menu: ctx
            .menu
            .borrow()
            .rows()
            .iter()
            .map(|row| wire::Launcher {
                app: row.app.clone(),
                label: row.label.clone(),
            })
            .collect(),
        desktop: ctx.icons.borrow().iter().map(launcher).collect(),
        tray: tray_entries(ctx),
    }
}
