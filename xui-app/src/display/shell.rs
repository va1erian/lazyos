//! The shell half of `os.lazy.display.v1` (issue #157): the calls only the
//! task holding the `shell` subscription may make, and the shell events the
//! compositor sends on that subscription's endpoint.
//!
//! Every call is bounded by [`SHELL_TICKS`]: the shell must keep its own UI
//! responsive when the compositor is slow, and an older compositor that does
//! not know a method answers `EINVAL` at once, which callers treat as "not
//! available" and degrade.

use libmessenger::Parcel;
use messenger_generated::os_lazy_display_v1 as wire;

use super::{request, Client};
use crate::sys::{self, errno};

/// PIT ticks (100 Hz) a shell call may wait for its reply: two seconds.
const SHELL_TICKS: u64 = 200;
/// Reply buffer for `ListSurfaces`: dozens of rows with 128-byte titles.
const LIST_BYTES: usize = 64 * 1024;

/// One `SurfaceChanged` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceChange {
    pub surface: u64,
    /// A `wire::CHANGE_*` value.
    pub kind: u32,
    pub minimized: bool,
    pub focused: bool,
    /// Set on `Created` and `Title`.
    pub title: Option<String>,
    /// A `wire::ROLE_*` value.
    pub role: u32,
}

/// One event on the shell subscription's endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellEvent {
    SurfaceChanged(SurfaceChange),
    /// The focused surface, or `None` when nothing has focus.
    FocusChanged(Option<u64>),
    /// Ctrl+Esc or Super: toggle the start menu.
    StartMenu,
    /// A press outside every panel: close popups.
    Dismiss,
}

/// Decode a shell event, or `None` for anything else (an unknown or
/// malformed message is dropped, never guessed at).
pub fn decode_shell_event(parcel: &Parcel) -> Option<ShellEvent> {
    let body = &parcel.body;
    Some(match parcel.header.method {
        wire::METHOD_SURFACECHANGED => {
            let args = wire::decode_surface_changed_args(body).ok()?;
            ShellEvent::SurfaceChanged(SurfaceChange {
                surface: args.surface,
                kind: args.kind,
                minimized: args.minimized,
                focused: args.focused,
                title: args.title,
                role: args.role,
            })
        }
        wire::METHOD_FOCUSCHANGED => {
            ShellEvent::FocusChanged(wire::decode_focus_changed_args(body).ok()?.surface)
        }
        wire::METHOD_STARTMENU => ShellEvent::StartMenu,
        wire::METHOD_DISMISS => ShellEvent::Dismiss,
        _ => return None,
    })
}

impl Client {
    /// `Subscribe(role)`, moving `events` (this task's peer end of a fresh
    /// pair) to the compositor. With `"shell"` this task becomes the shell.
    pub fn subscribe(&self, role: &str, events: u64) -> Result<(), i64> {
        let body = wire::encode_subscribe_args(&wire::SubscribeArgs {
            subscriber_role: role.into(),
        })
        .map_err(|_| -errno::EINVAL)?;
        self.shell_call(request(
            wire::METHOD_SUBSCRIBE,
            body,
            vec![events],
            Vec::new(),
        ))
        .map(|_| ())
    }

    /// `ListSurfaces`: every surface, bottom of the z-order first.
    pub fn list_surfaces(&self) -> Result<Vec<wire::SurfaceRow>, i64> {
        let parcel = request(
            wire::METHOD_LISTSURFACES,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let mut buf = vec![0u8; LIST_BYTES];
        let reply = self.call_into(&parcel, &mut buf, deadline())?;
        wire::decode_list_surfaces_reply(&reply.body)
            .map(|reply| reply.surfaces)
            .map_err(|_| -errno::EINVAL)
    }

    /// `GetWorkArea` as `(x, y, w, h)`.
    pub fn get_work_area(&self) -> Result<(i32, i32, i32, i32), i64> {
        let parcel = request(wire::METHOD_GETWORKAREA, Vec::new(), Vec::new(), Vec::new());
        let reply = self.shell_call(parcel)?;
        let area = wire::decode_get_work_area_reply(&reply.body).map_err(|_| -errno::EINVAL)?;
        Ok((area.x, area.y, area.w, area.h))
    }

    /// `SetWorkArea`: the rectangle windows may use.
    pub fn set_work_area(&self, x: i32, y: i32, w: i32, h: i32) -> Result<(), i64> {
        let body = wire::encode_set_work_area_args(&wire::SetWorkAreaArgs { x, y, w, h })
            .map_err(|_| -errno::EINVAL)?;
        self.unit(wire::METHOD_SETWORKAREA, body)
    }

    /// `PlaceSurface`: move a panel this task created to screen `(x, y)`.
    pub fn place_surface(&self, surface: u64, x: i32, y: i32) -> Result<(), i64> {
        let body = wire::encode_place_surface_args(&wire::PlaceSurfaceArgs { surface, x, y })
            .map_err(|_| -errno::EINVAL)?;
        self.unit(wire::METHOD_PLACESURFACE, body)
    }

    /// `ActivateSurface`: restore, raise and focus a window.
    pub fn activate_surface(&self, surface: u64) -> Result<(), i64> {
        let body = wire::encode_activate_surface_args(&wire::ActivateSurfaceArgs { surface })
            .map_err(|_| -errno::EINVAL)?;
        self.unit(wire::METHOD_ACTIVATESURFACE, body)
    }

    /// `MinimizeSurface`.
    pub fn minimize_surface(&self, surface: u64) -> Result<(), i64> {
        let body = wire::encode_minimize_surface_args(&wire::MinimizeSurfaceArgs { surface })
            .map_err(|_| -errno::EINVAL)?;
        self.unit(wire::METHOD_MINIMIZESURFACE, body)
    }

    /// `SetIconGeometry`: where `surface`'s taskbar entry is on screen (an
    /// empty rectangle forgets it).
    pub fn set_icon_geometry(&self, surface: u64, rect: (i32, i32, i32, i32)) -> Result<(), i64> {
        let (x, y, w, h) = rect;
        let body = wire::encode_set_icon_geometry_args(&wire::SetIconGeometryArgs {
            surface,
            x,
            y,
            w,
            h,
        })
        .map_err(|_| -errno::EINVAL)?;
        self.unit(wire::METHOD_SETICONGEOMETRY, body)
    }

    /// `HintLaunchOrigin`: the next window any task opens zooms from this
    /// screen rectangle.
    pub fn hint_launch_origin(&self, rect: (i32, i32, u32, u32)) -> Result<(), i64> {
        let (x, y, w, h) = rect;
        let body = wire::encode_hint_launch_origin_args(&wire::HintLaunchOriginArgs { x, y, w, h })
            .map_err(|_| -errno::EINVAL)?;
        self.unit(wire::METHOD_HINTLAUNCHORIGIN, body)
    }

    /// A bounded call whose reply carries no fields.
    fn unit(&self, method: u32, body: Vec<u8>) -> Result<(), i64> {
        self.shell_call(request(method, body, Vec::new(), Vec::new()))
            .map(|_| ())
    }

    fn shell_call(&self, parcel: Parcel) -> Result<Parcel, i64> {
        self.call_until(&parcel, deadline())
    }
}

/// The absolute tick a shell call gives up at.
fn deadline() -> u64 {
    sys::clock_ticks().saturating_add(SHELL_TICKS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(method: u32, body: Vec<u8>) -> Parcel {
        request(method, body, Vec::new(), Vec::new())
    }

    #[test]
    fn surface_changed_decodes_with_its_role_and_title() {
        let body = wire::encode_surface_changed_args(&wire::SurfaceChangedArgs {
            surface: 7,
            kind: wire::CHANGE_CREATED,
            x: 1,
            y: 2,
            w: 3,
            h: 4,
            minimized: false,
            focused: true,
            title: Some("Files".into()),
            role: wire::ROLE_WINDOW,
            maximized: false,
        })
        .unwrap();
        assert_eq!(
            decode_shell_event(&event(wire::METHOD_SURFACECHANGED, body)),
            Some(ShellEvent::SurfaceChanged(SurfaceChange {
                surface: 7,
                kind: wire::CHANGE_CREATED,
                minimized: false,
                focused: true,
                title: Some("Files".into()),
                role: wire::ROLE_WINDOW,
            }))
        );
    }

    #[test]
    fn focus_start_menu_and_dismiss_decode() {
        let body =
            wire::encode_focus_changed_args(&wire::FocusChangedArgs { surface: Some(3) }).unwrap();
        assert_eq!(
            decode_shell_event(&event(wire::METHOD_FOCUSCHANGED, body)),
            Some(ShellEvent::FocusChanged(Some(3)))
        );
        let none =
            wire::encode_focus_changed_args(&wire::FocusChangedArgs { surface: None }).unwrap();
        assert_eq!(
            decode_shell_event(&event(wire::METHOD_FOCUSCHANGED, none)),
            Some(ShellEvent::FocusChanged(None))
        );
        assert_eq!(
            decode_shell_event(&event(wire::METHOD_STARTMENU, Vec::new())),
            Some(ShellEvent::StartMenu)
        );
        assert_eq!(
            decode_shell_event(&event(wire::METHOD_DISMISS, Vec::new())),
            Some(ShellEvent::Dismiss)
        );
    }

    #[test]
    fn other_or_malformed_messages_are_not_shell_events() {
        assert_eq!(
            decode_shell_event(&event(wire::METHOD_POINTERMOVE, Vec::new())),
            None
        );
        assert_eq!(
            decode_shell_event(&event(wire::METHOD_SURFACECHANGED, vec![1, 2, 3])),
            None
        );
    }
}
