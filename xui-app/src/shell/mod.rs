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
//! `SHELL:POWER:*` ([`power`]).

mod ctx;
mod deskdir;
mod deskicons;
mod desktop;
mod heartbeat;
mod icons;
mod link;
mod menu;
mod power;
mod probe;
mod service;
mod services;
mod submenu;
mod taskbar;
mod theme;
mod wallpaper;

use std::rc::Rc;

use xui_core::app::run_app;
use xui_core::backend::Backend;

use crate::backend::LazyOSBackend;
use crate::client_window::SurfaceRole;
use crate::sys;

pub use service::NAME as SERVICE_NAME;

/// Pause before retrying a start-up step the compositor refused.
const RETRY_MILLIS: u64 = 5000;
/// Pause between attempts to reach the compositor at all.
const CONNECT_RETRY_MILLIS: u64 = 1000;

/// Run the shell until the compositor goes away; the process exit code.
pub fn run() -> i32 {
    let backend = connect();
    let Some(client) = backend.display_client() else {
        return 1;
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
        return 1;
    }
    // The shell's own uid: the service lets root and this user in. Unknown
    // (`None`) refuses every call rather than guessing an identity.
    let uid = sys::cred_get(None).ok().map(|cred| cred.uid);
    let ctx = Rc::new(ctx::Ctx::new(
        Rc::clone(&backend),
        client,
        screen,
        uid,
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
    ctx.reload_desktop(true);

    loop {
        backend.set_next_role(SurfaceRole::Desktop);
        let built = Rc::clone(&ctx);
        let outcome = run_app(
            Rc::clone(&backend) as Rc<dyn Backend>,
            desktop::spec(&ctx),
            move |ui| desktop::DesktopApp::build(built, windows, ui),
        );
        match outcome {
            Ok(()) => return 0,
            // An older compositor without the desktop role, or a refusal:
            // say so once, then keep trying without spinning.
            Err(error) => {
                ctx.note("desktop", || format!("SHELL:UP:FAIL {error}"));
                sys::sleep_millis(RETRY_MILLIS);
            }
        }
    }
}

/// Resolve the compositor, waiting for it to appear.
fn connect() -> Rc<LazyOSBackend> {
    let mut logged = false;
    loop {
        match LazyOSBackend::new_client() {
            Ok(backend) => return Rc::new(backend),
            Err(code) => {
                if !logged {
                    println!("SHELL:CONNECT:RETRY err={}", -code);
                    logged = true;
                }
                sys::sleep_millis(CONNECT_RETRY_MILLIS);
            }
        }
    }
}
