//! [`Client`]: an app's connection to the compositor: the generated
//! `os.lazy.display.v1` client with [`Rect`]s for the wire's separate
//! coordinates and the surface roles named.

use alloc::vec::Vec;

use libmessenger::client::{CallError, Connection};
use libmessenger::Buffer;
use messenger_generated::os_lazy_display_cursor_v1 as cursor;

use super::canvas::Rect;
use super::events::{SurfaceInfo, Theme};
use super::wire;
use crate::messenger::client::Nested;
use crate::messenger::Endpoint;

type Result<T> = core::result::Result<T, CallError>;

/// An app's connection to the compositor.
pub struct Client {
    wire: wire::Client<Nested>,
    /// The cursor scope's client: the same service name, another interface,
    /// resolved on first use.
    cursor: cursor::Client<Nested>,
}

/// Clamp a caller's size to the wire's `u32`; the compositor bounds it to the
/// screen anyway, so an oversized claim is refused there.
fn wire_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// A rectangle as the wire's unsigned fields (a negative size is empty).
fn wire_rect(rect: Rect) -> wire::Rect {
    wire::Rect {
        x: rect.x.max(0) as u32,
        y: rect.y.max(0) as u32,
        w: rect.w.max(0) as u32,
        h: rect.h.max(0) as u32,
    }
}

impl Client {
    /// The generated client underneath, for a call this wrapper has no
    /// method for.
    pub fn wire(&self) -> &wire::Client<Nested> {
        &self.wire
    }

    /// Resolve [`super::NAME`], waiting the runtime's connect bound
    /// ([`crate::messenger::client::CONNECT_BOUND`]) while the compositor
    /// starts.
    pub fn connect() -> Result<Client> {
        Ok(Client {
            wire: wire::Client::connect(Nested)?,
            cursor: cursor::Client::from_connection(Connection::new(Nested, cursor::SERVICE_NAME)),
        })
    }

    /// A `CreateSurface(width, height, title, events)` request. The event
    /// endpoint is moved to the compositor, which sends input back on it.
    /// Returns the new surface id.
    pub fn create_surface(
        &self,
        width: u64,
        height: u64,
        title: &str,
        events: &Endpoint,
    ) -> Result<u64> {
        self.create_surface_role(width, height, title, events, wire::ROLE_WINDOW)
    }

    /// A `CreateSurface` with [`wire::ROLE_DESKTOP`] (issue #167): the
    /// surface paints at the bottom of the z-order, above the background
    /// colour and below every window. It has no chrome, never takes focus and
    /// never appears in the taskbar or the Alt+Tab cycle; creating a new
    /// desktop replaces the previous one. The event endpoint is still
    /// transferred, so a future desktop can receive input.
    pub fn create_desktop_surface(
        &self,
        width: u64,
        height: u64,
        title: &str,
        events: &Endpoint,
    ) -> Result<u64> {
        self.create_surface_role(width, height, title, events, wire::ROLE_DESKTOP)
    }

    /// A `CreateSurface` with [`wire::ROLE_PANEL`] (issue #157): a shell-only,
    /// chromeless surface painted above every window (a taskbar, a start
    /// menu). It opens at `(0, 0)`; move it with [`Client::place_surface`]
    /// before its first commit. It never takes focus but receives the pointer
    /// events over it.
    pub fn create_panel_surface(
        &self,
        width: u64,
        height: u64,
        title: &str,
        events: &Endpoint,
    ) -> Result<u64> {
        self.create_surface_role(width, height, title, events, wire::ROLE_PANEL)
    }

    /// The shared body of the `create_*surface` calls.
    fn create_surface_role(
        &self,
        width: u64,
        height: u64,
        title: &str,
        events: &Endpoint,
        role: u32,
    ) -> Result<u64> {
        let surface = self.wire.create_surface(
            wire_u32(width),
            wire_u32(height),
            title,
            role,
            None,
            events.handle(),
        )?;
        // Surface ids start at 1: a missing id is a malformed reply.
        if surface == 0 {
            return Err(CallError::Transport(libmessenger::client::Errno::BADMSG));
        }
        Ok(surface)
    }

    /// `Subscribe(role, events)`: register this task for the shell events
    /// (issues #167, #157). The event endpoint is moved to the compositor,
    /// which sends one-way [`super::ShellEvent`]s there. The role
    /// [`super::ROLE_SHELL`] makes this task *the* shell (privileged, or the
    /// session that owns the display); any other role is a privileged
    /// observer that never replaces the shell. Registering again replaces the
    /// endpoint.
    pub fn subscribe(&self, role: &str, events: &Endpoint) -> Result<()> {
        self.wire.subscribe(role, events.handle())
    }

    /// `ListSurfaces` (shell-only): every surface the compositor knows,
    /// bottom first (the desktop, the windows in z-order, the panels), with
    /// the composited geometry (issue #167).
    pub fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>> {
        self.wire.list_surfaces()
    }

    /// `GetWorkArea`: the rectangle windows may occupy: what the shell set
    /// with [`Client::set_work_area`], else the whole screen.
    pub fn get_work_area(&self) -> Result<Rect> {
        let area = self.wire.get_work_area()?;
        Ok(Rect::new(area.x, area.y, area.w, area.h))
    }

    /// `GetTheme`: xuid's current chrome palette, so the shell's own
    /// surfaces can match it (issue #167).
    pub fn get_theme(&self) -> Result<Theme> {
        Ok(Theme::from_reply(&self.wire.get_theme()?))
    }

    /// Share `buffer` (a handle from the `display` syscall's
    /// `create_buffer`) with the compositor as `surface`'s pixels. The
    /// sender keeps its handle and mapping; the compositor gains one.
    pub fn attach_buffer(&self, surface: u64, buffer: u64, len: u64) -> Result<()> {
        self.wire.attach_buffer(surface, Buffer::whole(buffer, len))
    }

    /// Tell the compositor the `damage` rectangle of `surface` is ready.
    pub fn commit(&self, surface: u64, damage: Rect) -> Result<()> {
        let damage = wire_rect(damage);
        self.wire
            .commit(surface, damage.x, damage.y, damage.w, damage.h)
    }

    /// `SetSizeHints`: declare `surface` resizable within the given content
    /// bounds (a `max` of 0 means the screen). Only the surface's creator may
    /// call it. An older compositor answers `EINVAL`; callers may ignore it,
    /// leaving the window fixed-size. A caller that opts in must handle
    /// [`super::Event::Configure`]: allocate a buffer of the new size, attach
    /// it (`attach_buffer`, or `attach_slot` on a non-current slot), and
    /// redraw; the compositor refuses an attach smaller than the new size.
    pub fn set_size_hints(
        &self,
        surface: u64,
        min_w: u32,
        min_h: u32,
        max_w: u32,
        max_h: u32,
    ) -> Result<()> {
        self.wire
            .set_size_hints(surface, min_w, min_h, max_w, max_h)
    }

    /// `AttachBufferSlot`: share `buffer` with the compositor as buffer slot
    /// `slot` (`0..surfbuf::MAX_SLOTS`) of `surface` (issue #361). Fails with
    /// `EBUSY` while `slot` is the surface's current buffer.
    pub fn attach_slot(&self, surface: u64, slot: u32, buffer: u64, len: u64) -> Result<()> {
        self.wire
            .attach_buffer_slot(surface, slot, Buffer::whole(buffer, len))
    }

    /// `Present` (issue #361): a one-way, pipelined commit. Makes `slot` the
    /// surface's current buffer and composites `damage` (an empty slice or
    /// more than [`surfbuf::MAX_DAMAGE`] rectangles means the whole surface).
    /// The compositor answers with [`super::FrameEvent`]s on the surface's
    /// event endpoint: `BufferRelease` for the slot this replaced, then
    /// `FrameDone(seq)`.
    pub fn present(&self, surface: u64, slot: u32, seq: u64, damage: &[Rect]) -> Result<()> {
        let damage: Vec<wire::Rect> = damage.iter().copied().map(wire_rect).collect();
        self.wire.present(surface, slot, seq, &damage)
    }

    /// Drop `surface`; the compositor forgets it and repaints.
    pub fn destroy_surface(&self, surface: u64) -> Result<()> {
        self.wire.destroy_surface(surface)
    }

    /// `DragStart(surface, token, mime)`: hand `surface`'s in-progress
    /// gesture to the compositor, which tracks the pointer and delivers a
    /// `Drop` carrying `token`. The payload is offered to `clipboardd`
    /// first (issue #145); the compositor never sees the bytes.
    pub fn drag_start(&self, surface: u64, token: u64, mime: &str) -> Result<()> {
        self.wire.drag_start(surface, token, mime)
    }

    /// `DragCancel(surface)`: cancel the drag that started at `surface`.
    pub fn drag_cancel(&self, surface: u64) -> Result<()> {
        self.wire.drag_cancel(surface)
    }

    /// `PlaceSurface`: move this task's panel so its top-left is at screen
    /// `(x, y)` (clamped on screen).
    pub fn place_surface(&self, surface: u64, x: i32, y: i32) -> Result<()> {
        self.wire.place_surface(surface, x, y)
    }

    /// `ActivateSurface` (shell-only): restore, raise and focus a window.
    pub fn activate_surface(&self, surface: u64) -> Result<()> {
        self.wire.activate_surface(surface)
    }

    /// `ActivateApp` (shell-only): restore, raise and focus every window of
    /// the app with package label `label` in `session`.
    pub fn activate_app(&self, label: u32, session: u64) -> Result<()> {
        self.wire.activate_app(label, session)
    }

    /// `MinimizeSurface` (shell-only): minimize a window, like its title-bar
    /// button.
    pub fn minimize_surface(&self, surface: u64) -> Result<()> {
        self.wire.minimize_surface(surface)
    }

    /// `SetWorkArea` (shell-only): where windows may go, e.g. the screen
    /// minus the shell's taskbar.
    pub fn set_work_area(&self, area: Rect) -> Result<()> {
        self.wire.set_work_area(area.x, area.y, area.w, area.h)
    }

    /// `SetIconGeometry` (shell-only): where `surface`'s taskbar entry is,
    /// so its minimize/restore zoom flies there; an empty rect forgets it.
    pub fn set_icon_geometry(&self, surface: u64, rect: Rect) -> Result<()> {
        self.wire
            .set_icon_geometry(surface, rect.x, rect.y, rect.w, rect.h)
    }

    /// `HintLaunchOrigin` (shell-only): the next window any task opens soon
    /// zooms out of `rect` (the menu row or icon that launched it).
    pub fn hint_launch_origin(&self, rect: Rect) -> Result<()> {
        self.wire
            .hint_launch_origin(rect.x, rect.y, rect.w.max(0) as u32, rect.h.max(0) as u32)
    }

    /// Show `shape` (a `CursorShape` wire value) while the pointer is over
    /// `surface`'s content. The call travels on the cursor scope's interface
    /// id, so the kernel refuses an app whose package did not ask for the
    /// scope (`EACCES`); the compositor also checks that the task owns the
    /// surface.
    pub fn set_cursor(&self, surface: u64, shape: u32) -> Result<()> {
        self.cursor.set_cursor(surface, shape)
    }
}
