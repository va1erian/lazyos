//! Shell protocol support (issues #167, #175) and the Alt+Tab overlay
//! (issue #194 split), moved out of `xuid.rs` unchanged: the shell subscriber
//! and its event notifications, the fallback-taskbar visibility rule, and the
//! compositor-owned window cycle.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use user::messenger::display::{self, wire, Canvas, Rect};
use user::messenger::{self, Endpoint};

use super::drag::DragSession;
use super::render::repaint;
use super::surface::Surface;
use super::window::{restore, surface_by_id};

// ---------------------------------------------------------------------------
// Shell protocol (issue #167)
//
// LazyShell subscribes with Subscribe(role, events) and receives one-way
// SurfaceChanged/FocusChanged/StartMenu events; `"shell"` hides the built-in
// taskbar so the shell owns it. The desktop role is a CreateSurface flag. The
// Alt+Tab overlay is compositor-owned; the shell only sees the resulting
// FocusChanged/SurfaceChanged events.
// ---------------------------------------------------------------------------

/// A registered shell subscriber.
pub(super) struct ShellSub {
    /// Role string from `Subscribe`; [`display::ROLE_SHELL`] hides the bar.
    pub(super) role: String,
    /// Event endpoint handle in this task's table.
    pub(super) events: u64,
}

/// The modifier keys currently held, tracked from forwarded key codes.
#[derive(Clone, Copy, Default)]
pub(super) struct Modifiers {
    #[allow(dead_code)]
    pub(super) shift: bool,
    pub(super) ctrl: bool,
    pub(super) alt: bool,
    #[allow(dead_code)]
    pub(super) super_key: bool,
}

/// The open Alt+Tab overlay: a snapshot of the window cycle and the current
/// selection.
pub(super) struct AltTab {
    /// Visible window ids in cycle order (creation order).
    pub(super) order: Vec<u64>,
    /// Index into `order` of the highlighted entry.
    pub(super) selected: usize,
}

/// Whether the built-in taskbar paints: it stays the no-shell fallback and is
/// hidden once a `"shell"` subscriber registers.
pub(super) fn taskbar_visible(shell: Option<&ShellSub>) -> bool {
    shell
        .map(|shell| shell.role != display::ROLE_SHELL)
        .unwrap_or(true)
}

/// Set by [`notify_shell`] when the subscriber's event endpoint reports
/// `EPIPE` (issue #175): the shell process died without unsubscribing. The
/// main loop checks this after every event/request batch, drops the stale
/// subscription and repaints so the fallback taskbar returns.
static SHELL_DEAD: AtomicBool = AtomicBool::new(false);

/// Send one shell event to the subscriber. A closed peer (`EPIPE`) is
/// recorded in [`SHELL_DEAD`] instead of being silently ignored, so the
/// caller can drop the subscription (issue #175).
fn notify_shell(
    shell: Option<&ShellSub>,
    scratch: &mut Vec<u8>,
    method: u32,
    body: Result<Vec<u8>, libmessenger::Error>,
) {
    let Some(shell) = shell else {
        return;
    };
    let result = display::send_event(&Endpoint::from_raw(shell.events), scratch, method, body);
    if let Err(messenger::Error::Errno(code)) = result {
        if code == -messenger::errno::EPIPE {
            SHELL_DEAD.store(true, Ordering::Relaxed);
        }
    }
}

/// Tell the shell a surface changed (created/destroyed/moved/minimized/
/// restored; `kind` is a `wire::CHANGE_*`). Created events carry the title;
/// every row carries the composited geometry and flags.
pub(super) fn notify_surface(
    shell: Option<&ShellSub>,
    scratch: &mut Vec<u8>,
    surface: &Surface,
    focused: Option<u64>,
    kind: u32,
) {
    let args = wire::SurfaceChangedArgs {
        surface: surface.id,
        kind,
        x: surface.x,
        y: surface.y,
        w: surface.w,
        h: surface.h,
        minimized: surface.minimized,
        focused: focused == Some(surface.id),
        title: (kind == wire::CHANGE_CREATED).then(|| surface.title.clone()),
        role: surface.role(),
    };
    notify_shell(
        shell,
        scratch,
        wire::METHOD_SURFACECHANGED,
        wire::encode_surface_changed_args(&args),
    );
}

/// Tell the shell a surface is gone.
pub(super) fn notify_destroyed(shell: Option<&ShellSub>, scratch: &mut Vec<u8>, id: u64) {
    let args = wire::SurfaceChangedArgs {
        surface: id,
        kind: wire::CHANGE_DESTROYED,
        ..wire::SurfaceChangedArgs::default()
    };
    notify_shell(
        shell,
        scratch,
        wire::METHOD_SURFACECHANGED,
        wire::encode_surface_changed_args(&args),
    );
}

/// Tell the shell which surface is focused (`None` = none).
pub(super) fn notify_focus(shell: Option<&ShellSub>, scratch: &mut Vec<u8>, focused: Option<u64>) {
    let args = wire::FocusChangedArgs { surface: focused };
    notify_shell(
        shell,
        scratch,
        wire::METHOD_FOCUSCHANGED,
        wire::encode_focus_changed_args(&args),
    );
}

/// Forward the global start-menu hotkey to the shell.
pub(super) fn notify_start_menu(shell: Option<&ShellSub>, scratch: &mut Vec<u8>) {
    notify_shell(shell, scratch, wire::METHOD_STARTMENU, Ok(Vec::new()));
}

/// If a notification since the last check found the shell subscriber's
/// endpoint closed ([`SHELL_DEAD`]), drop the subscription and repaint the
/// full screen so the fallback taskbar returns and `GetWorkArea` reports the
/// full window rectangle again (issue #175).
pub(super) fn reap_dead_shell(
    shell: &mut Option<ShellSub>,
    surfaces: &[Surface],
    screen: &mut Canvas,
    pointer: (i32, i32),
    focused: Option<u64>,
    drag_session: Option<&DragSession>,
    alt_tab: Option<&AltTab>,
) {
    if !SHELL_DEAD.swap(false, Ordering::Relaxed) || shell.take().is_none() {
        return;
    }
    let full = Rect::new(0, 0, screen.width(), screen.height());
    // The subscription is already gone, so the fallback taskbar is visible.
    repaint(
        screen,
        surfaces,
        pointer,
        focused,
        full,
        drag_session,
        true,
        alt_tab,
    );
}
/// Whether a key code is one of the modifier keys the compositor consumes.
pub(super) fn modifier_key(key: u32) -> bool {
    matches!(
        key,
        display::key::SHIFT | display::key::CTRL | display::key::ALT | display::key::SUPER
    )
}

/// Open (or advance) the Alt+Tab overlay. The first Tab snapshots the visible
/// windows and selects the one after the current focus; repeated Tabs cycle.
pub(super) fn alt_tab_open(
    alt_tab: &mut Option<AltTab>,
    surfaces: &[Surface],
    focused: &Option<u64>,
    screen: &mut Canvas,
    pointer: (i32, i32),
    taskbar: bool,
) {
    match alt_tab {
        Some(tab) if !tab.order.is_empty() => {
            tab.selected = (tab.selected + 1) % tab.order.len();
        }
        _ => {
            let order: Vec<u64> = surfaces
                .iter()
                .filter(|surface| !surface.desktop && !surface.minimized)
                .map(|surface| surface.id)
                .collect();
            if order.is_empty() {
                return;
            }
            let current = focused.and_then(|id| order.iter().position(|&entry| entry == id));
            let selected = match current {
                Some(index) => (index + 1) % order.len(),
                None => 0,
            };
            *alt_tab = Some(AltTab { order, selected });
        }
    }
    let full = Rect::new(0, 0, screen.width(), screen.height());
    repaint(
        screen,
        surfaces,
        pointer,
        *focused,
        full,
        None,
        taskbar,
        alt_tab.as_ref(),
    );
}

/// Commit the Alt+Tab selection: restore, raise, and focus it, and tell the
/// shell about the resulting state changes.
#[allow(clippy::too_many_arguments)]
pub(super) fn alt_tab_commit(
    tab: &AltTab,
    surfaces: &mut Vec<Surface>,
    focused: &mut Option<u64>,
    screen: &mut Canvas,
    pointer: (i32, i32),
    shell: Option<&ShellSub>,
    scratch: &mut Vec<u8>,
    taskbar: bool,
) {
    let Some(id) = tab.order.get(tab.selected).copied() else {
        return;
    };
    if surface_by_id(surfaces, id).is_none() {
        return;
    }
    let before = *focused;
    let was_minimized = surface_by_id(surfaces, id).is_some_and(|surface| surface.minimized);
    restore(surfaces, focused, id);
    if was_minimized {
        if let Some(surface) = surface_by_id(surfaces, id) {
            notify_surface(shell, scratch, surface, *focused, wire::CHANGE_RESTORED);
        }
    }
    if *focused != before {
        notify_focus(shell, scratch, *focused);
    }
    let full = Rect::new(0, 0, screen.width(), screen.height());
    repaint(
        screen, surfaces, pointer, *focused, full, None, taskbar, None,
    );
}
