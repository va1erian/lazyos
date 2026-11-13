//! The LazyShell window-management calls (issue #157, `display.midl` methods
//! 36-41): the shell draws the taskbar and start menu itself, so what the old
//! in-compositor taskbar did with compositor state it now asks for here.
//!
//! Everything but `PlaceSurface` (which only a panel's creator may call) is
//! shell-only: the task holding the `shell` subscription, or a privileged
//! identity, which each call checks (`require_shell`) before it acts. Arguments are
//! untrusted: rectangles are clipped to the screen by the same 64-bit
//! `origin::translate` the open-origin hints use, and the pure rules are
//! boot-tested.

use alloc::vec::Vec;

use libmessenger::server::Fault;
use user::messenger::display::Rect;
use user::messenger::errno;
use user::sys;

use super::compositor::Compositor;
use super::origin::translate;
use super::protocol::refuse;
use super::surface::Surface;
use super::window::surface_by_id;

/// Where a `size` panel asked to sit at `at` lands: kept wholly on `screen`
/// (at its top-left edge when it is larger than the screen).
pub(super) fn clamp_panel(size: (i32, i32), at: (i32, i32), screen: Rect) -> (i32, i32) {
    let axis = |pos: i32, extent: i32, limit: i32| pos.min(limit - extent).max(0);
    (axis(at.0, size.0, screen.w), axis(at.1, size.1, screen.h))
}

/// The windows `ActivateApp` names: those created under package label `label` in `session`, in
/// creation order. A desktop or panel is never one, and label 0 (an unlabelled program) names
/// nothing.
pub(super) fn app_windows(surfaces: &[Surface], label: u32, session: u64) -> Vec<u64> {
    if label == 0 {
        return Vec::new();
    }
    surfaces
        .iter()
        .filter(|surface| {
            surface.is_window() && surface.label == label && surface.session == session
        })
        .map(|surface| surface.id)
        .collect()
}

/// Who asks for the shell role, as the rule needs it.
#[derive(Clone, Copy)]
pub(super) struct ShellClaim {
    /// A system service (`CAP_SETUID`; never a uid, issue #623).
    pub(super) privileged: bool,
    /// An installed (labelled) app: never the shell.
    pub(super) labelled: bool,
    pub(super) session: u64,
    /// The task already holds the live subscription (a re-subscribe).
    pub(super) current: bool,
}

/// The shell authorization rule (issues #157, #623): a privileged identity
/// may always be the shell. Otherwise the caller is an unlabelled task of a
/// real session, and either already the shell, or the shell is gone and its
/// session owns the display (a restarted LazyShell) or no session does yet. A
/// live shell is never displaced by another task, even of its own session: an
/// app of the session could otherwise read every window's title and focus.
pub(super) fn shell_allowed(
    claim: ShellClaim,
    display_session: Option<u64>,
    shell_live: bool,
) -> bool {
    if claim.privileged {
        return true;
    }
    if claim.labelled || claim.session == 0 {
        return false;
    }
    if claim.current {
        return true;
    }
    !shell_live && display_session.is_none_or(|owner| owner == claim.session)
}

impl Compositor {
    /// `(x, y, w, h)` clipped to the screen; `None` when nothing is left.
    pub(super) fn on_screen(&self, x: i32, y: i32, w: i64, h: i64) -> Option<Rect> {
        let size = |v: i64| u32::try_from(v.max(0)).unwrap_or(u32::MAX);
        translate(Rect::new(0, 0, 0, 0), (x, y, size(w), size(h)), self.full())
    }

    /// The window `id` for activate/minimize: `ENOENT` when unknown, `EINVAL`
    /// for a desktop or panel.
    pub(super) fn window_arg(&self, id: u64) -> Result<(), Fault> {
        match surface_by_id(&self.surfaces, id) {
            None => Err(refuse(errno::ENOENT)),
            Some(surface) if !surface.is_window() => Err(refuse(errno::EINVAL)),
            Some(_) => Ok(()),
        }
    }

    /// Where a new window of `owner` opens from: its own `HintOpenOrigin`
    /// first, else the shell's pending launch hint. Both are consumed.
    pub(super) fn take_open_origin(&mut self, owner: u64) -> Option<Rect> {
        let now = sys::clock();
        let own = super::origin::take(&mut self.hints, owner, now);
        if own.is_some() {
            // The task's own hint wins and leaves the shell's launch hint for
            // the window the shell actually launched.
            return own;
        }
        self.launch_hint
            .take()
            .filter(|hint| hint.expires > now)
            .map(|hint| hint.from)
    }
}

/// Boot check of the pure shell-call rules: `XUID:SHELLCALLS:PASS` or
/// `XUID:SHELLCALLS:FAIL`.
pub(super) fn selftest_shell_calls() -> &'static str {
    let screen = Rect::new(0, 0, 800, 600);
    let placed = clamp_panel((200, 32), (700, 590), screen) == (600, 568)
        && clamp_panel((200, 32), (-5, -5), screen) == (0, 0)
        && clamp_panel((900, 700), (50, 50), screen) == (0, 0);
    // Privilege always; the owning session once its shell is gone, or the
    // live shell itself; never another task while the shell lives, another
    // session, session 0 or an installed app.
    let claim = |session: u64| ShellClaim {
        privileged: false,
        labelled: false,
        session,
        current: false,
    };
    let privileged = ShellClaim {
        privileged: true,
        ..claim(0)
    };
    let current = ShellClaim {
        current: true,
        ..claim(7)
    };
    let app = ShellClaim {
        labelled: true,
        ..claim(9)
    };
    let auth = shell_allowed(privileged, Some(7), true)
        && shell_allowed(current, Some(7), true)
        && shell_allowed(claim(7), Some(7), false)
        && !shell_allowed(claim(7), Some(7), true)
        && !shell_allowed(claim(8), Some(7), false)
        && shell_allowed(claim(9), None, false)
        && !shell_allowed(claim(9), None, true)
        && !shell_allowed(app, None, false)
        && !shell_allowed(claim(0), None, false);
    // `ActivateApp` names the windows of one app in one session, and only windows.
    let window = |id: u64, label: u32, session: u64| {
        let mut surface = super::window::test_surface(id, false, false);
        surface.label = label;
        surface.session = session;
        surface
    };
    let mut desktop = super::window::test_surface(5, false, true);
    desktop.label = 7;
    desktop.session = 2;
    let surfaces = [
        window(1, 7, 2),
        window(2, 7, 2),
        window(3, 8, 2),
        window(4, 7, 9),
        desktop,
        window(6, 0, 0),
    ];
    let app = app_windows(&surfaces, 7, 2) == [1, 2]
        && app_windows(&surfaces, 8, 2) == [3]
        && app_windows(&surfaces, 7, 3).is_empty()
        && app_windows(&surfaces, 0, 0).is_empty();
    if placed && auth && app {
        "XUID:SHELLCALLS:PASS\n"
    } else {
        "XUID:SHELLCALLS:FAIL\n"
    }
}
