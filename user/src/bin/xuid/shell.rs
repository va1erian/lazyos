//! Shell protocol support (issues #167, #175, #157) and the Alt+Tab overlay
//! (issue #194 split): the shell and observer subscribers and their event
//! notifications, and the compositor-owned window cycle.

use alloc::vec::Vec;
use user::messenger::display::{self, wire};
use user::messenger::{self, Endpoint, Message};

use super::compositor::Compositor;
use super::window::{restore, surface_by_id};

// ---------------------------------------------------------------------------
// Shell protocol
//
// LazyShell subscribes with Subscribe("shell", events) and receives one-way
// SurfaceChanged/FocusChanged/StartMenu/Dismiss events; it draws the whole
// desktop UI (taskbar, start menu, wallpaper) on its own desktop and panel
// surfaces (issue #157). A privileged task may also subscribe under any other
// role as an observer, which gets the same events but never displaces the
// shell (issue #447). The Alt+Tab overlay is compositor-owned, so focus
// switching works (and cannot be spoofed) with no shell at all.
// ---------------------------------------------------------------------------

/// A registered subscriber: the shell, or the observer.
pub(super) struct ShellSub {
    /// Event endpoint handle in this task's table.
    pub(super) events: u64,
    /// The subscribing task (kernel-stamped sender slot).
    pub(super) task: u64,
    /// Set when a send to `events` reported `EPIPE`: the subscriber died
    /// without unsubscribing (issue #175). Dropped by the main loop.
    pub(super) dead: bool,
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
    /// Super went down and no other key since: its release opens the start
    /// menu (a chord such as Super+B does not).
    pub(super) super_alone: bool,
}

/// The open Alt+Tab overlay: a snapshot of the window cycle and the current
/// selection.
pub(super) struct AltTab {
    /// Window ids in cycle order (creation order), minimized ones included
    /// so they stay reachable without a shell.
    pub(super) order: Vec<u64>,
    /// Index into `order` of the highlighted entry.
    pub(super) selected: usize,
}

/// Send one event to `sub`, recording a closed peer (`EPIPE`) as death.
fn send_to(
    sub: Option<&mut ShellSub>,
    scratch: &mut Vec<u8>,
    method: u32,
    body: Result<Vec<u8>, libmessenger::Error>,
) {
    let Some(sub) = sub else {
        return;
    };
    let result = display::send_event(&Endpoint::from_raw(sub.events), scratch, method, body);
    if matches!(result, Err(messenger::Error::Errno(code)) if code == -messenger::errno::EPIPE) {
        sub.dead = true;
    }
}

impl Compositor {
    /// Send one shell event to the shell and the observer. The body is only
    /// copied when there is an observer (the bump allocator never reclaims).
    pub(super) fn notify_shell(&mut self, method: u32, body: Result<Vec<u8>, libmessenger::Error>) {
        if self.observer.is_some() {
            send_to(
                self.observer.as_mut(),
                &mut self.scratch,
                method,
                body.clone(),
            );
        }
        send_to(self.shell.as_mut(), &mut self.scratch, method, body);
    }

    /// Tell the shell surface `id` changed (created/destroyed/moved/minimized/
    /// restored; `kind` is a `wire::CHANGE_*`). Created events carry the
    /// title; every row carries the composited geometry and flags.
    pub(super) fn notify_surface(&mut self, id: u64, kind: u32) {
        let Some(surface) = surface_by_id(&self.surfaces, id) else {
            return;
        };
        let args = wire::SurfaceChangedArgs {
            surface: surface.id,
            kind,
            x: surface.x,
            y: surface.y,
            w: surface.w,
            h: surface.h,
            minimized: surface.minimized,
            focused: self.focused == Some(surface.id),
            title: matches!(kind, wire::CHANGE_CREATED | wire::CHANGE_TITLE)
                .then(|| surface.title.clone()),
            role: surface.role,
            maximized: surface.maximized.is_some(),
        };
        super::probe::window(surface);
        self.notify_shell(
            wire::METHOD_SURFACECHANGED,
            wire::encode_surface_changed_args(&args),
        );
    }

    /// Tell the shell a surface is gone.
    pub(super) fn notify_destroyed(&mut self, id: u64) {
        let args = wire::SurfaceChangedArgs {
            surface: id,
            kind: wire::CHANGE_DESTROYED,
            ..wire::SurfaceChangedArgs::default()
        };
        self.notify_shell(
            wire::METHOD_SURFACECHANGED,
            wire::encode_surface_changed_args(&args),
        );
    }

    /// Tell the shell which surface is focused (`None` = none).
    pub(super) fn notify_focus(&mut self) {
        let args = wire::FocusChangedArgs {
            surface: self.focused,
        };
        self.notify_shell(
            wire::METHOD_FOCUSCHANGED,
            wire::encode_focus_changed_args(&args),
        );
    }

    /// Forward the global start-menu hotkey to the shell (nothing happens
    /// without one: `xuid` has no menu of its own since issue #157).
    pub(super) fn notify_start_menu(&mut self) {
        self.notify_shell(wire::METHOD_STARTMENU, Ok(Vec::new()));
    }

    /// Tell the shell a press landed outside its panels, so it closes any
    /// popup (the start menu).
    pub(super) fn notify_dismiss(&mut self) {
        self.notify_shell(wire::METHOD_DISMISS, Ok(Vec::new()));
    }

    /// Whether `sender` may use the shell-only calls (`ListSurfaces`, the
    /// window-management methods, desktop and panel surfaces): the task that
    /// holds the shell subscription, or a privileged identity.
    pub(super) fn is_shell_caller(&self, message: &Message) -> bool {
        self.shell
            .as_ref()
            .is_some_and(|shell| shell.task == message.sender && !shell.dead)
            || super::protocol::is_privileged(message)
    }

    /// Drop the subscribers whose endpoint was found closed. Losing the shell
    /// returns the work area to the whole screen; its desktop and panels are
    /// reaped like any dead client's surfaces, and every window stays.
    pub(super) fn reap_dead_shell(&mut self) {
        if self.observer.as_ref().is_some_and(|sub| sub.dead) {
            if let Some(sub) = self.observer.take() {
                let _ = Endpoint::from_raw(sub.events).close();
            }
        }
        if self.shell.as_ref().is_some_and(|sub| sub.dead) {
            self.replace_shell(None);
        }
    }

    /// Install `next` as the shell subscriber (or none), closing the endpoint
    /// it replaces. A different task (or none) taking over resets the work
    /// area the old shell set and re-fits maximized windows to it.
    pub(super) fn replace_shell(&mut self, next: Option<ShellSub>) {
        let next_task = next.as_ref().map(|sub| sub.task);
        let Some(previous) = core::mem::replace(&mut self.shell, next) else {
            return;
        };
        // The panel keys were the old shell's: a new one starts without.
        if next_task != Some(previous.task) {
            self.set_panel_keys(false);
        }
        let _ = Endpoint::from_raw(previous.events).close();
        if next_task != Some(previous.task) && self.work.take().is_some() {
            self.reflow_maximized();
            self.repaint_full();
        }
    }

    /// Focus surface `id` (restoring and raising it), telling the shell about
    /// the restore and the focus change.
    pub(super) fn restore_and_focus(&mut self, id: u64) {
        // A window still opening lands first: it is hidden while it flies
        // in, which these state changes would misread as minimized.
        self.finish_opening();
        let before = self.focused;
        let was_minimized =
            surface_by_id(&self.surfaces, id).is_some_and(|surface| surface.minimized);
        if was_minimized {
            self.deiconify(id);
        }
        restore(&mut self.surfaces, &mut self.focused, id);
        if was_minimized {
            self.notify_surface(id, wire::CHANGE_RESTORED);
        }
        if self.focused != before {
            self.notify_focus();
        }
    }

    /// Open (or advance) the Alt+Tab overlay. The first Tab snapshots the
    /// windows (minimized ones too) and selects the one after the current
    /// focus; repeated Tabs cycle.
    pub(super) fn alt_tab_open(&mut self) {
        match &mut self.alt_tab {
            Some(tab) if !tab.order.is_empty() => {
                tab.selected = (tab.selected + 1) % tab.order.len();
            }
            _ => {
                let order: Vec<u64> = self
                    .surfaces
                    .iter()
                    .filter(|surface| surface.is_window())
                    .map(|surface| surface.id)
                    .collect();
                if order.is_empty() {
                    return;
                }
                let current = self
                    .focused
                    .and_then(|id| order.iter().position(|&entry| entry == id));
                let selected = match current {
                    Some(index) => (index + 1) % order.len(),
                    None => 0,
                };
                self.alt_tab = Some(AltTab { order, selected });
            }
        }
        self.repaint_full();
    }

    /// Commit the Alt+Tab selection: restore, raise, and focus it, and tell
    /// the shell about the resulting state changes.
    pub(super) fn alt_tab_commit(&mut self, tab: &AltTab) {
        let Some(id) = tab.order.get(tab.selected).copied() else {
            return;
        };
        if surface_by_id(&self.surfaces, id).is_none() {
            return;
        }
        self.restore_and_focus(id);
        self.repaint_full();
    }
}

/// Whether a key code is one of the modifier keys the compositor consumes.
pub(super) fn modifier_key(key: u32) -> bool {
    matches!(
        key,
        display::key::SHIFT | display::key::CTRL | display::key::ALT | display::key::SUPER
    )
}
