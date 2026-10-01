//! The LazyShell window-management calls (issue #157, `display.midl` methods
//! 36-41): the shell draws the taskbar and start menu itself, so what the old
//! in-compositor taskbar did with compositor state it now asks for here.
//!
//! Everything but `PlaceSurface` (which only a panel's creator may call) is
//! shell-only: the task holding the `shell` subscription, or a privileged
//! identity, which `handle_request` checks before dispatching. Arguments are
//! untrusted: rectangles are clipped to the screen by the same 64-bit
//! `origin::translate` the open-origin hints use, and the pure rules are
//! boot-tested.

use libmessenger::Parcel;
use user::messenger::display::{wire, Rect};
use user::messenger::{self, Message};
use user::sys;

use super::compositor::Compositor;
use super::origin::{translate, OpenHint, HINT_TTL_TICKS};
use super::protocol::{empty_reply, error_reply};
use super::window::surface_by_id;

/// Where a `size` panel asked to sit at `at` lands: kept wholly on `screen`
/// (at its top-left edge when it is larger than the screen).
pub(super) fn clamp_panel(size: (i32, i32), at: (i32, i32), screen: Rect) -> (i32, i32) {
    let axis = |pos: i32, extent: i32, limit: i32| pos.min(limit - extent).max(0);
    (axis(at.0, size.0, screen.w), axis(at.1, size.1, screen.h))
}

/// The shell authorization rule (issue #157): a privileged identity may
/// always be the shell; otherwise the caller needs a real session that owns
/// the display, or, before any session does, a display with no live shell.
pub(super) fn shell_allowed(
    privileged: bool,
    session: u64,
    display_session: Option<u64>,
    shell_live: bool,
) -> bool {
    privileged
        || (session != 0
            && match display_session {
                Some(owner) => owner == session,
                None => !shell_live,
            })
}

impl Compositor {
    /// `(x, y, w, h)` clipped to the screen; `None` when nothing is left.
    fn on_screen(&self, x: i32, y: i32, w: i64, h: i64) -> Option<Rect> {
        let size = |v: i64| u32::try_from(v.max(0)).unwrap_or(u32::MAX);
        translate(Rect::new(0, 0, 0, 0), (x, y, size(w), size(h)), self.full())
    }

    /// The window `id` for activate/minimize: `ENOENT` when unknown, `EINVAL`
    /// for a desktop or panel.
    fn window_arg(&self, id: u64) -> Result<(), i64> {
        match surface_by_id(&self.surfaces, id) {
            None => Err(messenger::errno::ENOENT),
            Some(surface) if !surface.is_window() => Err(messenger::errno::EINVAL),
            Some(_) => Ok(()),
        }
    }

    /// `PlaceSurface`: move a panel; only its creator may.
    pub(super) fn place_surface(&mut self, message: &Message, body: &[u8]) -> Parcel {
        let Ok(args) = wire::decode_place_surface_args(body) else {
            return error_reply(message.method(), messenger::errno::EINVAL);
        };
        let full = self.full();
        let Some(surface) = self.surfaces.iter_mut().find(|s| s.id == args.surface) else {
            return error_reply(message.method(), messenger::errno::ENOENT);
        };
        if surface.owner != message.sender {
            return error_reply(message.method(), messenger::errno::EACCES);
        }
        if !surface.is_panel() {
            return error_reply(message.method(), messenger::errno::EINVAL);
        }
        let before = surface.window();
        (surface.x, surface.y) = clamp_panel((surface.w, surface.h), (args.x, args.y), full);
        let after = surface.window();
        if after != before {
            self.notify_surface(args.surface, wire::CHANGE_MOVED);
            self.repaint(before.union(after));
        }
        empty_reply(message.method())
    }

    /// `ActivateSurface`: restore, raise and focus a window (a taskbar click).
    pub(super) fn activate_surface(&mut self, message: &Message, body: &[u8]) -> Parcel {
        let id = wire::decode_activate_surface_args(body)
            .unwrap_or_default()
            .surface;
        if let Err(code) = self.window_arg(id) {
            return error_reply(message.method(), code);
        }
        self.restore_and_focus(id);
        self.repaint_full();
        empty_reply(message.method())
    }

    /// `MinimizeSurface`: exactly the title-bar minimize button.
    pub(super) fn minimize_request(&mut self, message: &Message, body: &[u8]) -> Parcel {
        let id = wire::decode_minimize_surface_args(body)
            .unwrap_or_default()
            .surface;
        if let Err(code) = self.window_arg(id) {
            return error_reply(message.method(), code);
        }
        if surface_by_id(&self.surfaces, id).is_some_and(|surface| !surface.minimized) {
            self.minimize_surface(id);
        }
        empty_reply(message.method())
    }

    /// `SetWorkArea`: where windows may go (the screen minus the shell's
    /// taskbar); maximized windows are re-fitted to it.
    pub(super) fn set_work_area(&mut self, message: &Message, body: &[u8]) -> Parcel {
        let Ok(args) = wire::decode_set_work_area_args(body) else {
            return error_reply(message.method(), messenger::errno::EINVAL);
        };
        let full = self.full();
        let Some(area) = self.on_screen(args.x, args.y, args.w.into(), args.h.into()) else {
            return error_reply(message.method(), messenger::errno::EINVAL);
        };
        if self.work_area() != area {
            self.work = (area != full).then_some(area);
            self.reflow_maximized();
            self.repaint_full();
        }
        empty_reply(message.method())
    }

    /// `SetIconGeometry`: where a window's taskbar entry is, for the
    /// minimize/restore zoom; an empty rectangle forgets it.
    pub(super) fn set_icon_geometry(&mut self, message: &Message, body: &[u8]) -> Parcel {
        let Ok(args) = wire::decode_set_icon_geometry_args(body) else {
            return error_reply(message.method(), messenger::errno::EINVAL);
        };
        let icon = self.on_screen(args.x, args.y, args.w.into(), args.h.into());
        let Some(surface) = self.surfaces.iter_mut().find(|s| s.id == args.surface) else {
            return error_reply(message.method(), messenger::errno::ENOENT);
        };
        surface.icon = icon;
        empty_reply(message.method())
    }

    /// `HintLaunchOrigin`: the next window any task creates soon zooms open
    /// from this rectangle (a start-menu row, a desktop icon).
    pub(super) fn hint_launch_origin(&mut self, message: &Message, body: &[u8]) -> Parcel {
        let Ok(args) = wire::decode_hint_launch_origin_args(body) else {
            return error_reply(message.method(), messenger::errno::EINVAL);
        };
        if let Some(from) = self.on_screen(args.x, args.y, args.w.into(), args.h.into()) {
            self.launch_hint = Some(OpenHint {
                owner: message.sender,
                from,
                expires: sys::clock() + HINT_TTL_TICKS,
            });
        }
        empty_reply(message.method())
    }

    /// Where a new window of `owner` opens from: its own `HintOpenOrigin`
    /// first, else the shell's pending launch hint. Both are consumed.
    pub(super) fn take_open_origin(&mut self, owner: u64) -> Option<Rect> {
        let now = sys::clock();
        let own = super::origin::take(&mut self.hints, owner, now);
        let launch = self
            .launch_hint
            .take()
            .filter(|hint| hint.expires > now)
            .map(|hint| hint.from);
        own.or(launch)
    }
}

/// Boot check of the pure shell-call rules: `XUID:SHELLCALLS:PASS` or
/// `XUID:SHELLCALLS:FAIL`.
pub(super) fn selftest_shell_calls() -> &'static str {
    let screen = Rect::new(0, 0, 800, 600);
    let placed = clamp_panel((200, 32), (700, 590), screen) == (600, 568)
        && clamp_panel((200, 32), (-5, -5), screen) == (0, 0)
        && clamp_panel((900, 700), (50, 50), screen) == (0, 0);
    // Privilege always; the owning session; the first session only while no
    // shell is live; never session 0 or another session.
    let auth = shell_allowed(true, 0, Some(7), true)
        && shell_allowed(false, 7, Some(7), true)
        && !shell_allowed(false, 8, Some(7), false)
        && shell_allowed(false, 9, None, false)
        && !shell_allowed(false, 9, None, true)
        && !shell_allowed(false, 0, None, false);
    if placed && auth {
        "XUID:SHELLCALLS:PASS\n"
    } else {
        "XUID:SHELLCALLS:FAIL\n"
    }
}
