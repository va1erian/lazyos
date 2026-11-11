//! The shell half of `os.lazy.display.v1` (issue #157): the calls only the
//! task holding the `shell` subscription may make, and the shell events the
//! compositor sends on that subscription's endpoint.
//!
//! Every call is bounded by the `.midl` deadline (two seconds): the shell must
//! keep its own UI responsive when the compositor is slow, and an older compositor that does
//! not know a method answers `EINVAL` at once, which callers treat as "not
//! available" and degrade.

use libmessenger::Parcel;
use messenger_generated::os_lazy_display_v1 as wire;

#[cfg(test)]
use super::request;
use super::Client;
#[cfg(test)]
use crate::sys;

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
    /// A key (modifiers OR-ed in) while the shell holds the panel keys
    /// (`GrabPanelKeys`, issue #648).
    PanelKey(u32),
    /// Super+B: move the keyboard to the tray.
    TrayKeys,
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
        wire::METHOD_PANELKEY => ShellEvent::PanelKey(wire::decode_panel_key_args(body).ok()?.key),
        wire::METHOD_TRAYKEYS => ShellEvent::TrayKeys,
        _ => return None,
    })
}

impl Client {
    /// `Subscribe(role)`, moving `events` (this task's peer end of a fresh
    /// pair) to the compositor. With `"shell"` this task becomes the shell.
    pub fn subscribe(&self, role: &str, events: u64) -> Result<(), i64> {
        Ok(self.wire()?.subscribe(role, events)?)
    }

    /// `ListSurfaces`: every surface, bottom of the z-order first.
    pub fn list_surfaces(&self) -> Result<Vec<wire::SurfaceRow>, i64> {
        Ok(self.wire()?.list_surfaces()?)
    }

    /// `GetWorkArea` as `(x, y, w, h)`.
    pub fn get_work_area(&self) -> Result<(i32, i32, i32, i32), i64> {
        let area = self.wire()?.get_work_area()?;
        Ok((area.x, area.y, area.w, area.h))
    }

    /// `SetWorkArea`: the rectangle windows may use.
    pub fn set_work_area(&self, x: i32, y: i32, w: i32, h: i32) -> Result<(), i64> {
        Ok(self.wire()?.set_work_area(x, y, w, h)?)
    }

    /// `PlaceSurface`: move a panel this task created to screen `(x, y)`.
    pub fn place_surface(&self, surface: u64, x: i32, y: i32) -> Result<(), i64> {
        Ok(self.wire()?.place_surface(surface, x, y)?)
    }

    /// `ActivateSurface`: restore, raise and focus a window.
    pub fn activate_surface(&self, surface: u64) -> Result<(), i64> {
        Ok(self.wire()?.activate_surface(surface)?)
    }

    /// `ActivateApp`: restore, raise and focus every window of the app (package label `label`,
    /// `session`) that a click on one of its toasts names. `-ENOENT` when it has none.
    pub fn activate_app(&self, label: u32, session: u64) -> Result<(), i64> {
        Ok(self.wire()?.activate_app(label, session)?)
    }

    /// `MinimizeSurface`.
    pub fn minimize_surface(&self, surface: u64) -> Result<(), i64> {
        Ok(self.wire()?.minimize_surface(surface)?)
    }

    /// `SetIconGeometry`: where `surface`'s taskbar entry is on screen (an
    /// empty rectangle forgets it).
    pub fn set_icon_geometry(&self, surface: u64, rect: (i32, i32, i32, i32)) -> Result<(), i64> {
        let (x, y, w, h) = rect;
        Ok(self.wire()?.set_icon_geometry(surface, x, y, w, h)?)
    }

    /// `HintLaunchOrigin`: the next window any task opens zooms from this
    /// screen rectangle.
    pub fn hint_launch_origin(&self, rect: (i32, i32, u32, u32)) -> Result<(), i64> {
        let (x, y, w, h) = rect;
        Ok(self.wire()?.hint_launch_origin(x, y, w, h)?)
    }

    /// `GrabPanelKeys`: take (or give back) the keyboard for the shell's
    /// panel menus; keys then arrive as [`ShellEvent::PanelKey`].
    pub fn grab_panel_keys(&self, grab: bool) -> Result<(), i64> {
        Ok(self.wire()?.grab_panel_keys(grab)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(method: u32, body: Vec<u8>) -> Parcel {
        request(method, body, Vec::new())
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
    fn panel_keys_and_the_tray_chord_decode() {
        let body = wire::encode_panel_key_args(&wire::PanelKeyArgs {
            key: sys::key::PAGE_UP,
        })
        .unwrap();
        assert_eq!(
            decode_shell_event(&event(wire::METHOD_PANELKEY, body)),
            Some(ShellEvent::PanelKey(sys::key::PAGE_UP))
        );
        assert_eq!(
            decode_shell_event(&event(wire::METHOD_TRAYKEYS, Vec::new())),
            Some(ShellEvent::TrayKeys)
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
