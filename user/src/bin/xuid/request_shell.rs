//! Drag & drop and shell-protocol requests (issue #194 split): `DragStart`,
//! `DragCancel`, `Subscribe` (with the shell authorization rule of issues
//! #157 and #447), `ListSurfaces`, `GetWorkArea` and `GetTheme`. The
//! LazyShell window-management calls (methods 36-41) live in `shellcalls.rs`.

use alloc::string::String;
use libmessenger::server::{Caller, Fault};
use user::messenger::display::{self, wire};
use user::messenger::{errno, Endpoint};

use super::compositor::Compositor;
use super::protocol::{color_u32, privileged, refuse};
use super::shell::ShellSub;
use super::shellcalls::{shell_allowed, ShellClaim};
use super::theme::{accent, border_color, mode, taskbar_bg, title_bg, title_bg_focus, title_text};

impl Compositor {
    /// `Subscribe`: register the shell, or a privileged observer.
    ///
    /// The `shell` role is accepted from a privileged identity, or from an
    /// unlabelled task of the session that owns the display (the first
    /// non-zero session accepted as the shell, until it logs out) once the
    /// previous shell is gone, so the graphical session's LazyShell needs no
    /// capability and a restarted one replaces its predecessor; a live shell
    /// is never displaced (issue #623). Any other role is an observer: it
    /// needs privilege and can never displace the shell (issue #447).
    pub(super) fn subscribe(
        &mut self,
        cx: &Caller,
        role: String,
        events: u64,
    ) -> Result<(), Fault> {
        // Decoded, so the event endpoint is ours to close on a refusal.
        let refuse = |code| {
            let _ = Endpoint::from_raw(events).close();
            refuse(code)
        };
        if role.is_empty() || role.len() > display::MAX_ROLE {
            return Err(refuse(errno::EINVAL));
        }
        let sub = ShellSub {
            events,
            task: cx.sender,
            dead: false,
        };
        if role != display::ROLE_SHELL {
            if !privileged(cx) {
                return Err(refuse(errno::EACCES));
            }
            // A re-subscribe replaces the endpoint, so close the one it
            // replaces (issue #175: it was leaked).
            if let Some(previous) = self.observer.replace(sub) {
                let _ = Endpoint::from_raw(previous.events).close();
            }
            return Ok(());
        }
        if !self.may_be_shell(cx) {
            // Every window title, geometry and focus change is the shell's
            // (issue #175): anyone else's claim is refused outright.
            return Err(refuse(errno::EACCES));
        }
        if self.display_session.is_none() && cx.session != 0 {
            self.display_session = Some(cx.session);
        }
        self.replace_shell(Some(sub));
        // The desktop paints in the shell user's own theme (issue #407).
        if self.themefeed.follow_user(cx.uid) {
            self.repaint_full();
        }
        // And types with the user's own keyboard layout.
        self.layoutfeed.follow_user(cx.uid, self.themefeed.confd());
        Ok(())
    }

    /// Whether the task `cx` names may hold the shell role (see
    /// [`Compositor::subscribe`] and [`shell_allowed`]).
    fn may_be_shell(&self, cx: &Caller) -> bool {
        let live = self.shell.as_ref().filter(|shell| !shell.dead);
        let claim = ShellClaim {
            privileged: privileged(cx),
            labelled: cx.label_id != 0,
            session: cx.session,
            current: live.is_some_and(|shell| shell.task == cx.sender),
        };
        shell_allowed(claim, self.display_session, live.is_some())
    }
}

impl Compositor {}

/// `GetTheme`: the chrome palette.
pub(super) fn get_theme() -> wire::GetThemeReply {
    wire::GetThemeReply {
        title_bg_active: color_u32(title_bg_focus()),
        title_bg_inactive: color_u32(title_bg()),
        border: color_u32(border_color()),
        taskbar: color_u32(taskbar_bg()),
        text: color_u32(title_text()),
        mode: mode().as_str().into(),
        accent: color_u32(accent()),
    }
}
