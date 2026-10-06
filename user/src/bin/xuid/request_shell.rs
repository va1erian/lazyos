//! Drag & drop and shell-protocol requests (issue #194 split): `DragStart`,
//! `DragCancel`, `Subscribe` (with the shell authorization rule of issues
//! #157 and #447), `ListSurfaces`, `GetWorkArea` and `GetTheme`. The
//! LazyShell window-management calls (methods 36-41) live in `shellcalls.rs`.

use libmessenger::Parcel;
use user::messenger::display::{self, wire};
use user::messenger::{self, Endpoint, Message};
use user::sys::Cred;

use super::compositor::Compositor;
use super::protocol::{
    color_u32, drop_rejected_transfers, empty_reply, error_reply, privileged, typed_reply,
};
use super::shell::ShellSub;
use super::shellcalls::shell_allowed;
use super::surface::Surface;
use super::theme::{accent, border_color, mode, taskbar_bg, title_bg, title_bg_focus, title_text};
use super::window::surface_by_id;

impl Compositor {
    /// `DragStart`: begin a compositor-mediated drag from the owner's surface.
    pub(super) fn drag_start(&mut self, message: &Message, body: &[u8]) -> Parcel {
        let args = wire::decode_drag_start_args(body).unwrap_or_default();
        let (id, token, mime) = (args.surface, args.token, args.mime);
        if self.drag_session.is_some() {
            return error_reply(message.method(), messenger::errno::EBUSY);
        }
        let Some(surface) = surface_by_id(&self.surfaces, id) else {
            return error_reply(message.method(), messenger::errno::EINVAL);
        };
        // Only the surface's own client may drag from it, and only with a
        // pointer button held: the gesture is what makes it a drag.
        if surface.owner != message.sender {
            return error_reply(message.method(), messenger::errno::EACCES);
        }
        if token == 0 || mime.is_empty() || mime.len() > display::MAX_MIME || !self.button_down {
            return error_reply(message.method(), messenger::errno::EINVAL);
        }
        self.drag_begin(id, token, mime);
        empty_reply(message.method())
    }

    /// `DragCancel`: the source's client abandons its drag.
    pub(super) fn drag_cancel_request(&mut self, message: &Message, body: &[u8]) -> Parcel {
        let id = wire::decode_drag_cancel_args(body)
            .unwrap_or_default()
            .surface;
        let owns = self
            .drag_session
            .as_ref()
            .is_some_and(|active| active.source == id)
            && surface_by_id(&self.surfaces, id).is_some_and(|s| s.owner == message.sender);
        if owns {
            self.drag_cancel();
        }
        empty_reply(message.method())
    }

    /// `Subscribe`: register the shell, or a privileged observer.
    ///
    /// The `shell` role is accepted from a privileged identity, or from a
    /// task of the session that owns the display (the first non-zero
    /// session accepted as the shell), so the graphical session's
    /// LazyShell needs no capability and a restarted one replaces its
    /// predecessor. Any other role is an observer: it needs privilege and
    /// can never displace the shell (issue #447).
    pub(super) fn subscribe(&mut self, message: &Message, body: &[u8]) -> Parcel {
        let role = wire::decode_subscribe_args(body)
            .unwrap_or_default()
            .subscriber_role;
        if !message.carries(wire::SUBSCRIBE_TRANSFERS)
            || role.is_empty()
            || role.len() > display::MAX_ROLE
        {
            drop_rejected_transfers(message);
            return error_reply(message.method(), messenger::errno::EINVAL);
        }
        let cred = message.caller();
        let sub = ShellSub {
            events: message.first_handle,
            task: message.sender,
            dead: false,
        };
        if role != display::ROLE_SHELL {
            if !privileged(&cred) {
                drop_rejected_transfers(message);
                return error_reply(message.method(), messenger::errno::EACCES);
            }
            // A re-subscribe replaces the endpoint, so close the one it
            // replaces (issue #175: it was leaked).
            if let Some(previous) = self.observer.replace(sub) {
                let _ = Endpoint::from_raw(previous.events).close();
            }
            return empty_reply(message.method());
        }
        let Some(cred) = Some(cred).filter(|cred| self.may_be_shell(cred)) else {
            // Every window title, geometry and focus change is the shell's
            // (issue #175): anyone else's claim is refused outright.
            drop_rejected_transfers(message);
            return error_reply(message.method(), messenger::errno::EACCES);
        };
        if self.display_session.is_none() && cred.session != 0 {
            self.display_session = Some(cred.session);
        }
        self.replace_shell(Some(sub));
        empty_reply(message.method())
    }

    /// Whether `cred` may hold the shell role (see [`Compositor::subscribe`]
    /// and [`shell_allowed`]).
    fn may_be_shell(&self, cred: &Cred) -> bool {
        let live = self.shell.as_ref().is_some_and(|shell| !shell.dead);
        shell_allowed(privileged(cred), cred.session, self.display_session, live)
    }

    /// `ListSurfaces` (shell-only): one row per surface, bottom first: the
    /// desktop, the windows in z-order, then the panels.
    pub(super) fn list_surfaces(&self, message: &Message) -> Parcel {
        let layer = |keep: fn(&Surface) -> bool| self.surfaces.iter().filter(move |s| keep(s));
        let surfaces = layer(Surface::is_desktop)
            .chain(layer(Surface::is_window))
            .chain(layer(Surface::is_panel))
            .map(|surface| wire::SurfaceRow {
                id: surface.id,
                title: surface.title.clone(),
                x: surface.x,
                y: surface.y,
                w: surface.w,
                h: surface.h,
                minimized: surface.minimized,
                focused: self.focused == Some(surface.id),
                role: surface.role,
                maximized: surface.maximized.is_some(),
            })
            .collect();
        typed_reply(
            message.method(),
            wire::encode_list_surfaces_reply(&wire::ListSurfacesReply { surfaces }),
        )
    }

    /// `GetWorkArea`: the shell's work area, else the whole screen (which is
    /// how the shell learns the screen size before it sets one).
    pub(super) fn get_work_area(&self, message: &Message) -> Parcel {
        let area = self.work_area();
        typed_reply(
            message.method(),
            wire::encode_get_work_area_reply(&wire::GetWorkAreaReply {
                x: area.x,
                y: area.y,
                w: area.w,
                h: area.h,
            }),
        )
    }
}

impl Compositor {
    /// `GetOutput`: the screen in physical pixels and the UI scale
    /// (docs/hidpi-plan.md). Open to every client: nothing here is private.
    pub(super) fn get_output(&self, message: &Message) -> Parcel {
        typed_reply(
            message.method(),
            wire::encode_get_output_reply(&wire::GetOutputReply {
                width: self.screen.width() as u32,
                height: self.screen.height() as u32,
                scale: super::theme::scale() as u32,
            }),
        )
    }
}

/// `GetTheme`: the chrome palette.
pub(super) fn get_theme(message: &Message) -> Parcel {
    typed_reply(
        message.method(),
        wire::encode_get_theme_reply(&wire::GetThemeReply {
            title_bg_active: color_u32(title_bg_focus()),
            title_bg_inactive: color_u32(title_bg()),
            border: color_u32(border_color()),
            taskbar: color_u32(taskbar_bg()),
            text: color_u32(title_text()),
            mode: mode().as_str().into(),
            accent: color_u32(accent()),
        }),
    )
}
