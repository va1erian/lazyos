//! The shell's link to the compositor: the `shell` subscription and the
//! events it delivers.
//!
//! `Subscribe("shell")` must succeed before the desktop and panels can be
//! created (they are shell-only roles), so start-up retries it until the
//! compositor answers. Events are drained on every heartbeat without parking:
//! the channel counters say whether anything is queued.

use std::rc::Rc;

use lazyshell::taskbar::{ChangeKind, Delta, Role, Surface};
use messenger_generated::os_lazy_display_v1 as wire;
use xui_core::app::Ui;

use super::ctx::Ctx;
use super::menu;
use crate::display::{self, Client, ShellEvent, SurfaceChange};
use crate::sys::{self, errno, EXPIRED_DEADLINE};

/// Most events applied per heartbeat, so a flood cannot starve painting.
const EVENTS_PER_TICK: usize = 64;
/// Pause between subscription attempts.
const RETRY_MILLIS: u64 = 500;
/// Receive buffer for one shell event (a title is at most 128 bytes).
const EVENT_BYTES: usize = 4096;

/// Subscribe as the shell, retrying until the compositor accepts; returns
/// this task's end of the event channel. A refusal is logged once.
pub fn subscribe(client: Client) -> u64 {
    let mut logged = false;
    loop {
        match try_subscribe(client) {
            Ok(events) => return events,
            Err(code) => {
                if !logged {
                    println!("SHELL:SUBSCRIBE:RETRY err={}", -code);
                    logged = true;
                }
                sys::sleep_millis(RETRY_MILLIS);
            }
        }
    }
}

/// One attempt with a fresh pair (the peer handle moves with the call, so a
/// failed attempt's handles are not reused).
fn try_subscribe(client: Client) -> Result<u64, i64> {
    let (events, peer) = sys::msg_create_pair()?;
    match client.subscribe("shell", peer) {
        Ok(()) => Ok(events),
        Err(code) => {
            let _ = display::close(peer);
            let _ = display::close(events);
            Err(code)
        }
    }
}

/// The screen size: the work area's extent (the whole screen until a shell
/// sets one), grown to cover any desktop or panel surface still listed (a
/// restarted shell may find its predecessor's work area not yet reset).
pub fn screen_size(area: (i32, i32, i32, i32), rows: &[wire::SurfaceRow]) -> (i32, i32) {
    let mut size = (area.0 + area.2, area.1 + area.3);
    for row in rows.iter().filter(|row| row.role != wire::ROLE_WINDOW) {
        size.0 = size.0.max(row.x + row.w);
        size.1 = size.1.max(row.y + row.h);
    }
    size
}

/// The model's view of a protocol role.
fn role(role: u32) -> Role {
    if role == wire::ROLE_WINDOW {
        Role::Window
    } else {
        Role::Shell
    }
}

/// The model's view of a `ListSurfaces` row.
pub fn row_surface(row: &wire::SurfaceRow) -> Surface<'_> {
    Surface {
        id: row.id,
        role: role(row.role),
        title: Some(&row.title),
        minimized: row.minimized,
        focused: row.focused,
    }
}

fn change_kind(kind: u32) -> ChangeKind {
    match kind {
        wire::CHANGE_CREATED => ChangeKind::Created,
        wire::CHANGE_DESTROYED => ChangeKind::Destroyed,
        wire::CHANGE_MINIMIZED => ChangeKind::Minimized,
        wire::CHANGE_RESTORED => ChangeKind::Restored,
        wire::CHANGE_TITLE => ChangeKind::Title,
        _ => ChangeKind::Other,
    }
}

/// Apply one `SurfaceChanged` to the taskbar; prints the add/remove markers.
pub fn apply_change(ctx: &Ctx, change: &SurfaceChange) {
    let surface = Surface {
        id: change.surface,
        role: role(change.role),
        title: change.title.as_deref(),
        minimized: change.minimized,
        focused: change.focused,
    };
    let delta = ctx
        .taskbar
        .borrow_mut()
        .apply(change_kind(change.kind), &surface);
    match delta {
        Delta::None => return,
        Delta::Added(id, title) => println!("SHELL:TASKBAR:ADD id={id} title={title}"),
        Delta::Removed(id) => println!("SHELL:TASKBAR:REMOVE id={id}"),
        Delta::Changed => {}
    }
    ctx.bar_changed();
}

/// Drain the shell events queued since the last heartbeat.
pub fn pump<M: 'static>(ctx: &Rc<Ctx>, ui: &Ui<M>) {
    let mut buf = vec![0u8; EVENT_BYTES];
    for _ in 0..EVENTS_PER_TICK {
        match sys::msg_queued(ctx.events) {
            Ok(0) => return,
            Ok(_) => {}
            Err(code) => return lost(ctx, code),
        }
        let result = match sys::msg_recv(ctx.events, &mut buf, EXPIRED_DEADLINE) {
            Ok(result) => result,
            Err(code) if code == -errno::ETIMEDOUT => return,
            Err(code) => return lost(ctx, code),
        };
        let Some(event) = display::decode_message(&buf[..result.bytes as usize])
            .as_ref()
            .and_then(display::decode_shell_event)
        else {
            continue;
        };
        handle(ctx, ui, event);
    }
}

/// The event channel failed (the compositor died, or the channel broke): the
/// shell cannot follow the windows without it, so it exits and `init`
/// restarts it with a fresh subscription. Only a timeout is transient, and
/// `pump` handles that before calling here.
fn lost(ctx: &Ctx, code: i64) {
    ctx.note("link-lost", || format!("SHELL:LINK:LOST err={}", -code));
    std::process::exit(1);
}

fn handle<M: 'static>(ctx: &Rc<Ctx>, ui: &Ui<M>, event: ShellEvent) {
    match event {
        ShellEvent::SurfaceChanged(change) => apply_change(ctx, &change),
        ShellEvent::FocusChanged(focused) => {
            if ctx.taskbar.borrow_mut().set_focus(focused) {
                ctx.repaint_bar();
            }
        }
        ShellEvent::StartMenu => menu::toggle(ctx, ui),
        ShellEvent::Dismiss => menu::close(ctx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(role: u32, x: i32, y: i32, w: i32, h: i32) -> wire::SurfaceRow {
        wire::SurfaceRow {
            id: 1,
            title: String::new(),
            x,
            y,
            w,
            h,
            minimized: false,
            focused: false,
            role,
            maximized: false,
        }
    }

    #[test]
    fn the_screen_is_the_work_area_grown_by_shell_surfaces() {
        assert_eq!(screen_size((0, 0, 1024, 768), &[]), (1024, 768));
        // A predecessor's work area excluded its bar; its desktop row did not.
        let rows = [
            row(wire::ROLE_WINDOW, 100, 100, 2000, 2000),
            row(wire::ROLE_DESKTOP, 0, 0, 1024, 768),
            row(wire::ROLE_PANEL, 0, 736, 1024, 32),
        ];
        assert_eq!(screen_size((0, 0, 1024, 736), &rows), (1024, 768));
    }

    #[test]
    fn protocol_kinds_and_roles_map_onto_the_model() {
        assert_eq!(change_kind(wire::CHANGE_CREATED), ChangeKind::Created);
        assert_eq!(change_kind(wire::CHANGE_TITLE), ChangeKind::Title);
        assert_eq!(change_kind(wire::CHANGE_MAXIMIZED), ChangeKind::Other);
        assert_eq!(change_kind(999), ChangeKind::Other);
        assert_eq!(role(wire::ROLE_WINDOW), Role::Window);
        assert_eq!(role(wire::ROLE_PANEL), Role::Shell);
        assert_eq!(role(wire::ROLE_DESKTOP), Role::Shell);
    }
}
