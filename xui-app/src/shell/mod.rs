//! LazyShell (issue #157): the desktop shell as an xui client of `xuid`.
//!
//! The compositor keeps compositing, window policy and security; the shell
//! owns the desktop UI: the wallpaper ([`wallpaper`]) and launcher icons ([`desktop`], a
//! `ROLE_DESKTOP` surface), the taskbar with the "LazyOS" button, the window
//! entries and the clock ([`taskbar`], a panel), and the start menu ([`menu`],
//! a panel created on demand, with the restart / shut down rows). It
//! publishes `os.lazy.shell` ([`service`]).
//! The decisions themselves live in the host-tested `lazyshell` crate.
//!
//! Start-up order matters: `Subscribe("shell")` first (desktop and panel
//! surfaces are shell-only), then the screen size from `GetWorkArea` and the
//! existing windows from `ListSurfaces`, then the desktop, the taskbar and
//! `SetWorkArea`. A restarted shell rebuilds its taskbar from that list.
//!
//! Serial markers: `SHELL:UP:PASS`, `SHELL:SERVICE:PASS`,
//! `SHELL:DESKTOP:PASS icons=<n>`, `SHELL:MENU:OPEN`/`CLOSE`,
//! `SHELL:LAUNCH:PASS app=<id> pid=<pid>` / `FAIL app=<id> err=<errno>`,
//! `SHELL:TASKBAR:ADD id=<surface> title=<title>` / `REMOVE id=<surface>`,
//! `SHELL:RESTART:PASS windows=<n>`, `SHELL:WALLPAPER:PASS path=<path>
//! size=<w>x<h>` / `FAIL path=<path> <why>` / `NONE`, and the power rows'
//! `SHELL:POWER:*` ([`power`]), and the app-failure notice's
//! `SHELL:FAILURE` ([`failures`]) and the toasts' `SHELL:NOTIFY:*` / `SHELL:NOTICE:*`
//! ([`notify`]).

mod ctx;
mod deskdir;
mod deskicons;
mod deskscan;
mod desktop;
mod failures;
mod heartbeat;
mod icons;
mod keys;
mod link;
pub(crate) mod menu;
mod notify;
mod power;
mod probe;
mod service;
mod services;
/// `init.Launch(app, arg, 0)`: start an app in the shell's session (the
/// Terminal's New Window uses it).
pub use services::launch;
mod submenu;
mod taskbar;
mod theme;
mod tray;
mod wallpaper;

use std::rc::Rc;
use std::time::Duration;

use xui_core::app::run_app;
use xui_core::backend::Backend;

use crate::backend::LazyOSBackend;
use crate::client_window::SurfaceRole;
use crate::sys;
use crate::trace::Channel;

pub use service::NAME as SERVICE_NAME;

/// Pause before retrying a start-up step the compositor refused.
const RETRY_MILLIS: u64 = 5000;

/// Run the shell until the compositor goes away, then exit the process. The
/// shell is a compositor client from the start (it never owns the display)
/// and waits for the compositor to appear; `UP` is its first desktop frame.
pub fn run() -> ! {
    crate::app(Channel::Shell, "LazyShell")
        .client_only()
        .wait_for_display(Duration::MAX)
        .run_host(|backend, _| serve(&backend))
}

/// The shell's life on a connected backend: its taskbar, desktop and
/// services, until the desktop window closes.
fn serve(backend: &Rc<LazyOSBackend>) -> Result<(), String> {
    let Some(client) = backend.display_client() else {
        return Err(String::from("the display client is gone"));
    };
    let events = link::subscribe(client);
    let rows = client.list_surfaces().unwrap_or_else(|code| {
        println!("SHELL:LIST:FAIL err={}", -code);
        Vec::new()
    });
    let area = client.get_work_area().unwrap_or_else(|code| {
        println!("SHELL:WORKAREA:FAIL err={}", -code);
        (0, 0, 0, 0)
    });
    // The shell lays itself out in design pixels (docs/hidpi-plan.md): the
    // screen at the desktop's UI scale; `Ctx` converts at the protocol edge.
    let scale = backend.scale() as i32;
    let physical = link::screen_size(area, &rows);
    let screen = (physical.0 / scale, physical.1 / scale);
    if screen.0 <= 0 || screen.1 <= lazyshell::taskbar::BAR_H {
        println!("SHELL:UP:FAIL screen={}x{}", screen.0, screen.1);
        return Err(format!("no usable screen: {}x{}", screen.0, screen.1));
    }
    // The shell's own uid: the service lets root and this user in. Unknown
    // (`None`) refuses every call rather than guessing an identity.
    let cred = sys::cred_get(None).ok();
    let uid = cred.map(|cred| cred.uid);
    // Its session: only this session's app failures become notices.
    let session = cred.map(|cred| cred.session);
    let ctx = Rc::new(ctx::Ctx::new(
        Rc::clone(backend),
        client,
        screen,
        uid,
        session,
        events,
    ));
    let windows = ctx
        .taskbar
        .borrow_mut()
        .seed(rows.iter().map(link::row_surface));
    for window in ctx.taskbar.borrow().windows() {
        println!(
            "SHELL:TASKBAR:ADD id={} title={}",
            window.surface, window.title
        );
    }
    ctx.reload_menu();
    // The desktop folder is scanned by the worker `DesktopApp::build` starts:
    // on a slow volume the first scan (it also seeds the folder) took half a
    // minute, and the taskbar was missing for all of it.

    loop {
        backend.set_next_role(SurfaceRole::Desktop);
        let built = Rc::clone(&ctx);
        let outcome = run_app(
            Rc::clone(backend) as Rc<dyn Backend>,
            desktop::spec(&ctx),
            move |ui| desktop::DesktopApp::build(built, windows, ui),
        );
        match outcome {
            Ok(()) => return Ok(()),
            // An older compositor without the desktop role, or a refusal:
            // say so once, then keep trying without spinning.
            Err(error) => {
                ctx.note("desktop", || format!("SHELL:UP:FAIL {error}"));
                sys::sleep_millis(RETRY_MILLIS);
            }
        }
    }
}
