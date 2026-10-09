//! The LazyShell protocol checks (issue #157): panels, the work area, the
//! window-management calls, and the authorization negatives run in child
//! processes of the probe (`observer`, `denied <panel>`).

use alloc::format;
use user::messenger::display::{self, wire, Canvas, Client, Color, Event, Rect, ShellEvent};
use user::messenger::{self, Endpoint, Error};
use user::sys;

/// The session the probe claims the display for. A probe child in another
/// session must then be refused the shell role.
pub(super) const DISPLAY_SESSION: u64 = 4242;
/// Session id the unprivileged probe child runs under.
const OTHER_SESSION: u64 = 4243;
/// The probe panel's size.
const PANEL_W: i32 = 200;
const PANEL_H: i32 = 24;
/// The work area the probe sets: the screen minus a 32 px taskbar strip.
const BAR_H: i32 = 32;

/// A fresh endpoint pair, or `None` (logged) when the table is full.
fn pair(marker: &str) -> Option<(Endpoint, Endpoint)> {
    match messenger::create_pair() {
        Ok(pair) => Some(pair),
        Err(_) => {
            sys::write_str(&format!("SHELLPROBE:{marker}:FAIL:create_pair\n"));
            None
        }
    }
}

/// Whether `result` failed with exactly `-code`; logs a `FAIL` otherwise.
fn refused<T>(marker: &str, what: &str, result: Result<T, Error>, code: i64) -> bool {
    match result {
        Err(Error::Errno(got)) if got == -code => true,
        other => {
            let got = other.err().and_then(|error| error.errno());
            sys::write_str(&format!(
                "SHELLPROBE:{marker}:FAIL:{what} gave {got:?}, want -{code}\n"
            ));
            false
        }
    }
}

/// Whether `result` succeeded; logs a `FAIL` otherwise.
fn ok<T>(marker: &str, what: &str, result: Result<T, Error>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            let code = error.errno().unwrap_or(0);
            sys::write_str(&format!("SHELLPROBE:{marker}:FAIL:{what} gave {code}\n"));
            None
        }
    }
}

/// Log `SHELLPROBE:<marker>:PASS` when `passed`.
pub(super) fn verdict(marker: &str, passed: bool) -> bool {
    if passed {
        sys::write_str(&format!("SHELLPROBE:{marker}:PASS\n"));
    }
    passed
}

/// Claim the display for [`DISPLAY_SESSION`] (the probe stays uid 0, so it is
/// still privileged): the probe children then test the cross-session rule.
pub(super) fn claim_session() {
    if let Ok(mut cred) = sys::cred_get(None) {
        cred.session = DISPLAY_SESSION;
        let _ = sys::cred_set(None, &cred);
    }
}

/// A panel in the top-right corner: created at (0, 0), placed (an
/// off-screen request is clamped back on screen), painted and committed.
/// Returns its id (for the `denied` child's `PlaceSurface` negative) and
/// its event endpoint (the main loop logs the pointer events it gets).
pub(super) fn panel(client: &Client, screen: Rect) -> Option<(u64, Endpoint)> {
    let (events, published) = pair("PANEL")?;
    let panel = ok(
        "PANEL",
        "create",
        client.create_panel_surface(PANEL_W as u64, PANEL_H as u64, "probe panel", &published),
    )?;
    ok(
        "PANEL",
        "place",
        client.place_surface(panel, screen.w + 500, -50),
    )?;
    let bytes = (PANEL_W * PANEL_H * 4) as u64;
    let (buffer, va, _) = sys::buffer_create(bytes).ok()?;
    // SAFETY: `va` maps the buffer just created, `PANEL_W * PANEL_H * 4`
    // bytes long, and nothing else touches it.
    let mut canvas = unsafe { Canvas::new(va, PANEL_W, PANEL_H) };
    let area = Rect::new(0, 0, PANEL_W, PANEL_H);
    canvas.fill(area, area, Color::rgb(40, 24, 64));
    canvas.text(8, 8, "shellprobe panel", Color::rgb(240, 220, 255), area, 1);
    ok(
        "PANEL",
        "attach",
        client.attach_buffer(panel, buffer, bytes),
    )?;
    ok("PANEL", "commit", client.commit(panel, area))?;
    let rows = ok("PANEL", "list", client.list_surfaces())?;
    let placed = rows.iter().any(|row| {
        row.id == panel
            && row.role == wire::ROLE_PANEL
            && (row.x, row.y) == (screen.w - PANEL_W, 0)
            && !row.focused
    });
    // Panels are listed last, above every window.
    let last = rows.last().is_some_and(|row| row.id == panel);
    if !placed || !last {
        sys::write_str("SHELLPROBE:PANEL:FAIL:row\n");
    }
    verdict("PANEL", placed && last).then_some((panel, events))
}

/// `SetWorkArea` round-trips through `GetWorkArea`; an empty one is refused.
pub(super) fn work_area(client: &Client, screen: Rect) -> bool {
    let area = Rect::new(0, 0, screen.w, screen.h - BAR_H);
    let set = ok("WORKAREA", "set", client.set_work_area(area)).is_some();
    let read = client.get_work_area().is_ok_and(|got| got == area);
    let empty = refused(
        "WORKAREA",
        "empty",
        client.set_work_area(Rect::new(0, 0, 0, 0)),
        messenger::errno::EINVAL,
    );
    verdict("WORKAREA", set && read && empty)
}

/// A privileged child subscribes as a non-shell observer (issue #447): the
/// shell keeps its role, so the work area it set survives (a replaced shell
/// would have reset it), and its event channel still works (checked by
/// [`window_management`], which waits for a `SurfaceChanged` on it).
pub(super) fn evict(client: &Client, screen: Rect) -> bool {
    if sys::spawn_native(fhs::bin::SHELLPROBE, &["observer"]).is_none() {
        sys::write_str("SHELLPROBE:EVICT:FAIL:spawn\n");
        return false;
    }
    let status = sys::wait(sys::clock() + 500);
    let subscribed = status.is_some_and(|(_, status)| status == 0);
    let area = Rect::new(0, 0, screen.w, screen.h - BAR_H);
    let kept = client.get_work_area().is_ok_and(|got| got == area);
    if !subscribed || !kept {
        sys::write_str(&format!(
            "SHELLPROBE:EVICT:FAIL:observer={status:?} work area kept={kept}\n"
        ));
    }
    subscribed && kept
}

/// The observer child: a privileged `Subscribe` under another role must be
/// accepted, and must not displace the shell (the parent checks that).
pub(super) fn observer_child() -> ! {
    let (Ok(client), Some((_events, published))) = (Client::connect(), pair("EVICT")) else {
        sys::exit(1)
    };
    let subscribed = ok(
        "EVICT",
        "observer",
        client.subscribe("observer", &published),
    );
    sys::exit(if subscribed.is_some() { 0 } else { 1 })
}

/// Minimize, activate, icon geometry and the launch hint on the probe's own
/// window, with the negatives for the wrong roles and an unknown id. The
/// minimize must reach the shell channel as a `SurfaceChanged`.
/// Returns whether the checks passed and whether the shell channel delivered.
pub(super) fn window_management(
    client: &Client,
    shell: &Endpoint,
    ids: (u64, u64, u64),
) -> (bool, bool) {
    let (window, desktop, panel) = ids;
    let screen = client.get_work_area().unwrap_or(Rect::new(0, 0, 640, 480));
    let icon = Rect::new(4, screen.h + 3, 160, 26);
    let icon_set = ok("WM", "icon", client.set_icon_geometry(window, icon)).is_some();
    let minimized = ok("WM", "minimize", client.minimize_surface(window)).is_some()
        && row_flags(client, window) == Some((true, false));
    let heard = heard_minimize(shell, window);
    let activated = ok("WM", "activate", client.activate_surface(window)).is_some()
        && row_flags(client, window) == Some((false, true));
    let hinted = ok(
        "WM",
        "hint",
        client.hint_launch_origin(Rect::new(10, 10, 40, 40)),
    )
    .is_some();
    let einval = messenger::errno::EINVAL;
    let wrong_role = refused(
        "WM",
        "activate desktop",
        client.activate_surface(desktop),
        einval,
    ) && refused(
        "WM",
        "minimize panel",
        client.minimize_surface(panel),
        einval,
    );
    let unknown = refused(
        "WM",
        "icon of unknown",
        client.set_icon_geometry(u64::MAX, icon),
        messenger::errno::ENOENT,
    );
    let passed = icon_set && minimized && heard && activated && hinted && wrong_role && unknown;
    if !passed {
        sys::write_str(&format!(
            "SHELLPROBE:WM:FAIL:icon={icon_set} min={minimized} heard={heard} act={activated}\n"
        ));
    }
    (verdict("WM", passed), heard)
}

/// `(minimized, focused)` of `id`'s row.
fn row_flags(client: &Client, id: u64) -> Option<(bool, bool)> {
    let rows = client.list_surfaces().ok()?;
    let row = rows.iter().find(|row| row.id == id)?;
    Some((row.minimized, row.focused))
}

/// Whether the shell channel delivers `window`'s `Minimized` change within
/// two seconds. Other events are consumed; the main loop needs none of them.
fn heard_minimize(shell: &Endpoint, window: u64) -> bool {
    let mut buf = alloc::vec![0u8; 4096];
    let deadline = sys::clock() + 200;
    while sys::clock() < deadline {
        let Ok(message) = shell.recv_with(&mut buf, Some(deadline)) else {
            return false;
        };
        if let Some(ShellEvent::SurfaceChanged(change)) = display::decode_shell_event(&message) {
            if change.surface == window && change.kind == wire::CHANGE_MINIMIZED {
                return true;
            }
        }
    }
    false
}

/// Log the pointer events a desktop or panel endpoint received
/// (`SHELLPROBE:<tag>:DOWN|UP|LEAVE x,y`), so a session can check that the
/// compositor routes the pointer to the shell's layers. Moves inside are not
/// logged (there are many); the leave move at `(-1, -1)` is.
pub(super) fn log_layer(tag: &str, events: &Endpoint, buf: &mut [u8]) {
    while let Ok(Some(message)) = events.poll_recv_with(buf) {
        let line = match display::decode_event(&message) {
            Some(Event::PointerDown { x, y, button }) => format!("DOWN {x},{y} b{button}"),
            Some(Event::PointerUp { x, y, button }) => format!("UP {x},{y} b{button}"),
            Some(Event::PointerMove { x: -1, y: -1 }) => "LEAVE".into(),
            Some(Event::PointerWheel { delta, .. }) => format!("WHEEL {delta}"),
            _ => continue,
        };
        sys::write_str(&format!("SHELLPROBE:{tag}:{line}\n"));
    }
}

/// The unprivileged child (issues #175, #157): after dropping to a plain user
/// in another session it must be refused the shell role (the display belongs
/// to [`DISPLAY_SESSION`]), an observer slot, the desktop and the list
/// (`DENIED`), and every shell-only call plus moving the probe's panel
/// (`SHELLONLY`), each with `EACCES`.
pub(super) fn denied_child(panel: u64) -> ! {
    let plain = sys::Cred::new(1000, 1000, 0, 0, OTHER_SESSION);
    if sys::cred_set(None, &plain).is_err() {
        sys::write_str("SHELLPROBE:DENIED:FAIL:could not drop privilege\n");
        sys::exit(1);
    }
    let Ok(client) = Client::connect() else {
        sys::write_str("SHELLPROBE:DENIED:FAIL:connect\n");
        sys::exit(1)
    };
    // A refused call still consumes the transferred endpoint, so each gets
    // its own pair.
    let (Some(shell), Some(observer), Some(desktop), Some(panel_end)) = (
        pair("DENIED"),
        pair("DENIED"),
        pair("DENIED"),
        pair("DENIED"),
    ) else {
        sys::exit(1)
    };
    let eacces = messenger::errno::EACCES;
    let denied = refused(
        "DENIED",
        "shell",
        client.subscribe(display::ROLE_SHELL, &shell.1),
        eacces,
    ) && refused(
        "DENIED",
        "observer",
        client.subscribe("observer", &observer.1),
        eacces,
    ) && refused(
        "DENIED",
        "desktop",
        client.create_desktop_surface(64, 64, "evil", &desktop.1),
        eacces,
    ) && refused("DENIED", "list", client.list_surfaces(), eacces);
    let rect = Rect::new(0, 0, 64, 64);
    let shell_only = refused(
        "SHELLONLY",
        "panel",
        client.create_panel_surface(64, 64, "evil", &panel_end.1),
        eacces,
    ) && refused("SHELLONLY", "activate", client.activate_surface(1), eacces)
        && refused("SHELLONLY", "minimize", client.minimize_surface(1), eacces)
        && refused("SHELLONLY", "work area", client.set_work_area(rect), eacces)
        && refused(
            "SHELLONLY",
            "icon",
            client.set_icon_geometry(1, rect),
            eacces,
        )
        && refused(
            "SHELLONLY",
            "launch hint",
            client.hint_launch_origin(rect),
            eacces,
        )
        && refused(
            "SHELLONLY",
            "place",
            client.place_surface(panel, 0, 0),
            eacces,
        );
    let passed = verdict("DENIED", denied) & verdict("SHELLONLY", shell_only);
    sys::exit(if passed { 0 } else { 1 })
}
