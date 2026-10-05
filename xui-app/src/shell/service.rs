//! `os.lazy.shell` (`os.lazy.shell.v1`, idl/shell.midl): the shell's own
//! Messenger service, so scripts, tests and session programs can drive the
//! desktop the way a user would.
//!
//! Every call is checked against the sender's kernel-stamped uid (0 or the
//! shell's own; see `lazyshell::policy`): the uid the kernel stamped on the
//! message when it was queued (`Request::origin`), so the shell needs no
//! capability to identify callers. When its own uid is unknown it refuses
//! every call with `EACCES` (fail closed).

use std::rc::Rc;

use libmessenger::Parcel;
use messenger_generated::os_lazy_shell_v1 as wire;
use xui_core::app::Ui;

use super::ctx::Ctx;
use super::heartbeat::Heartbeat;
use super::menu;
use crate::server::{error_parcel, reply_parcel, Request, Server};
use crate::sys::errno;

/// The registered service name.
pub const NAME: &str = "os.lazy.shell";
/// Most requests answered per heartbeat.
const REQUESTS_PER_TICK: usize = 8;
/// Receive buffer for one request (the largest is a short app id).
const REQUEST_BYTES: usize = 4096;
/// `ENOTSUP`, for a method this version does not have.
const ENOTSUP: i64 = 95;

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
                    println!("SHELL:SERVICE:PASS");
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
            let reply = answer(ctx, ui, &request);
            if let Some(txn) = request.txn {
                let _ = server.reply(txn, &reply);
            }
        }
    }
}

/// The reply to one request: authorize, then dispatch.
fn answer<M: 'static>(ctx: &Rc<Ctx>, ui: &Ui<M>, request: &Request) -> Parcel {
    let method = request.parcel.header.method;
    if request.parcel.header.interface_id != wire::INTERFACE_ID {
        return error_parcel(wire::INTERFACE_ID, method, ENOTSUP, "not os.lazy.shell.v1");
    }
    let caller = request.origin.uid;
    if !lazyshell::policy::caller_allowed(Some(caller), ctx.uid) {
        ctx.note("deny", || format!("SHELL:SERVICE:DENY uid={caller}"));
        return error_parcel(
            wire::INTERFACE_ID,
            method,
            errno::EACCES,
            "caller not allowed",
        );
    }
    match dispatch(ctx, ui, method, &request.parcel.body) {
        Ok(body) => reply_parcel(wire::INTERFACE_ID, method, body),
        Err((code, text)) => error_parcel(wire::INTERFACE_ID, method, code, text),
    }
}

/// A failed call: positive errno and friendly text.
type Failure = (i64, &'static str);

fn dispatch<M: 'static>(
    ctx: &Rc<Ctx>,
    ui: &Ui<M>,
    method: u32,
    body: &[u8],
) -> Result<Vec<u8>, Failure> {
    let bad = (errno::EINVAL, "malformed request");
    match method {
        wire::METHOD_STATUS => encode(wire::encode_status_reply(&status(ctx))),
        wire::METHOD_SHOWSTARTMENU => {
            let args = wire::decode_show_start_menu_args(body).map_err(|_| bad)?;
            if args.open {
                menu::open(ctx, ui);
            } else {
                menu::close(ctx);
            }
            Ok(Vec::new())
        }
        wire::METHOD_LAUNCH => {
            let args = wire::decode_launch_args(body).map_err(|_| bad)?;
            if !deskmenu::valid_app_id(&args.app) {
                return Err((errno::EINVAL, "not an app id"));
            }
            let pid = ctx
                .launch(&args.app, launch_origin(ctx, &args.app))
                .map_err(|code| (code.abs(), "launch failed"))?;
            encode(wire::encode_launch_reply(&wire::LaunchReply { pid }))
        }
        wire::METHOD_REFRESH => {
            ctx.reload_menu();
            ctx.reload_desktop(true);
            ctx.repaint_menu();
            let reply = wire::RefreshReply {
                menu: ctx.menu.borrow().rows().len() as u32,
                desktop: ctx.icons.borrow().len() as u32,
            };
            encode(wire::encode_refresh_reply(&reply))
        }
        wire::METHOD_ACTIVATE => {
            let args = wire::decode_activate_args(body).map_err(|_| bad)?;
            if !ctx.taskbar.borrow().contains(args.surface) {
                return Err((errno::ENOENT, "no such taskbar entry"));
            }
            ctx.client
                .activate_surface(args.surface)
                .map_err(|code| (code.abs(), "activate failed"))?;
            Ok(Vec::new())
        }
        _ => Err((ENOTSUP, "unknown method")),
    }
}

fn encode<E>(body: Result<Vec<u8>, E>) -> Result<Vec<u8>, Failure> {
    body.map_err(|_| (errno::E2BIG, "reply too large"))
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
    }
}
