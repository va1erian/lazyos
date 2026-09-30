//! Drag & drop and shell-protocol requests (issue #194 split): `DragStart`,
//! `DragCancel`, `Subscribe`, `ListSurfaces`, `GetWorkArea` and `GetTheme`.

use libmessenger::Parcel;
use user::messenger::display::{self, wire};
use user::messenger::{self, Endpoint, Message};

use super::compositor::Compositor;
use super::protocol::{
    color_u32, drop_rejected_handle, empty_reply, error_reply, is_privileged, typed_reply,
};
use super::shell::ShellSub;
use super::theme::{BORDER_COLOR, TASKBAR_BG, TITLE_BG, TITLE_BG_FOCUS, TITLE_TEXT};
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

    /// `Subscribe`: register the (single) shell event subscriber.
    pub(super) fn subscribe(&mut self, message: &Message, body: &[u8]) -> Parcel {
        let role = wire::decode_subscribe_args(body)
            .unwrap_or_default()
            .subscriber_role;
        if message.handles == 0 || role.is_empty() || role.len() > display::MAX_ROLE {
            drop_rejected_handle(message);
            return error_reply(message.method(), messenger::errno::EINVAL);
        }
        if role == display::ROLE_SHELL && !is_privileged(message.sender) {
            // Only an authorized shell identity may hide the fallback taskbar
            // and receive every surface/focus event (issue #175); anyone
            // else's claim is refused outright.
            drop_rejected_handle(message);
            return error_reply(message.method(), messenger::errno::EACCES);
        }
        // One subscriber at a time; a re-subscribe replaces the endpoint, so
        // close the one it replaces (issue #175: it was leaked).
        let previous = self.shell.replace(ShellSub {
            role,
            events: message.first_handle,
        });
        if let Some(previous) = previous {
            let _ = Endpoint::from_raw(previous.events).close();
        }
        // Registering a shell can hide or reveal the fallback taskbar, which
        // changes the work area: re-fit maximized windows to it.
        self.reflow_maximized();
        self.repaint_full();
        empty_reply(message.method())
    }

    /// `ListSurfaces`: one row per surface, in z-order (privileged).
    pub(super) fn list_surfaces(&self, message: &Message) -> Parcel {
        if !is_privileged(message.sender) {
            // Every window title and geometry is compositor-privileged (issue
            // #175); a request from anyone else is refused outright.
            return error_reply(message.method(), messenger::errno::EACCES);
        }
        let surfaces = self
            .surfaces
            .iter()
            .map(|surface| wire::SurfaceRow {
                id: surface.id,
                title: surface.title.clone(),
                x: surface.x,
                y: surface.y,
                w: surface.w,
                h: surface.h,
                minimized: surface.minimized,
                focused: self.focused == Some(surface.id),
                role: surface.role(),
                maximized: surface.maximized.is_some(),
            })
            .collect();
        typed_reply(
            message.method(),
            wire::encode_list_surfaces_reply(&wire::ListSurfacesReply { surfaces }),
        )
    }

    /// `GetWorkArea`: with a shell registered the fallback bar is hidden, so
    /// windows may use the whole screen.
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

/// `GetTheme`: the chrome palette.
pub(super) fn get_theme(message: &Message) -> Parcel {
    typed_reply(
        message.method(),
        wire::encode_get_theme_reply(&wire::GetThemeReply {
            title_bg_active: color_u32(TITLE_BG_FOCUS),
            title_bg_inactive: color_u32(TITLE_BG),
            border: color_u32(BORDER_COLOR),
            taskbar: color_u32(TASKBAR_BG),
            text: color_u32(TITLE_TEXT),
        }),
    )
}
