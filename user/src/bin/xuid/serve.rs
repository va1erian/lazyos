//! `xuid`'s display requests: the generated server of `os.lazy.display.v1`
//! over the [`Compositor`] (`dispatch.rs` picks the interface).
//!
//! The generated `dispatch` decodes; each method here checks who asks (the
//! shell-only calls first, before they act) and calls the compositor.
//!
//! Events (pointer, keys, drag, shell and frame notices) travel from `xuid`
//! to a client, so a request carrying one is ignored.

use alloc::string::String;
use alloc::vec::Vec;
use libmessenger::server::{Caller, Fault};
use libmessenger::Buffer;
use user::messenger::display::{self, wire, Rect};
use user::messenger::errno;
use user::sys;

use super::compositor::Compositor;
use super::origin::{OpenHint, HINT_TTL_TICKS};
use super::present::attach;
use super::protocol::refuse;
use super::request_shell::get_theme;
use super::shellcalls::{app_windows, clamp_panel};
use super::surface::Surface;
use super::window::surface_by_id;

impl wire::Server for Compositor {
    fn create_surface(
        &mut self,
        cx: &Caller,
        width: u32,
        height: u32,
        title: String,
        role: u32,
        popup: Option<u64>,
        events: u64,
    ) -> Result<u64, Fault> {
        let args = wire::CreateSurfaceArgs {
            width,
            height,
            title,
            role,
            popup,
            events,
        };
        Compositor::create_surface(self, cx, args)
    }

    /// Map the client's pixel buffer as slot 0 and show it.
    fn attach_buffer(&mut self, cx: &Caller, id: u64, pixels: Buffer) -> Result<(), Fault> {
        attach(cx.sender, &mut self.surfaces, id, None, &pixels).map_err(refuse)?;
        self.repaint_full();
        Ok(())
    }

    /// Register one buffer slot (issue #361). It is not the current slot, so
    /// nothing on screen changes.
    fn attach_buffer_slot(
        &mut self,
        cx: &Caller,
        id: u64,
        slot: u32,
        pixels: Buffer,
    ) -> Result<(), Fault> {
        attach(cx.sender, &mut self.surfaces, id, Some(slot), &pixels).map_err(refuse)
    }

    /// The client's damaged rectangle is ready; repaint it.
    fn commit(
        &mut self,
        cx: &Caller,
        id: u64,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> Result<(), Fault> {
        let Some(surface) = self.surfaces.iter().find(|surface| surface.id == id) else {
            return Ok(());
        };
        if surface.owner != cx.sender {
            // Only the owner may commit damage (issue #176: any caller that
            // guessed the id could paint over it).
            return Err(refuse(errno::EACCES));
        }
        if surface.minimized {
            // The pixels are hidden; the minimize repaint already cleared the
            // screen area. Only the buffer changed.
            return Ok(());
        }
        // Damage is relative to the content origin (a chromeless surface's
        // content is all of it).
        let area = surface.content();
        let damage = Rect::new(
            area.x.saturating_add_unsigned(x),
            area.y.saturating_add_unsigned(y),
            i32::try_from(w).unwrap_or(i32::MAX),
            i32::try_from(h).unwrap_or(i32::MAX),
        )
        .intersect(area);
        self.repaint(damage);
        Ok(())
    }

    /// Only the owner may close its window.
    fn destroy_surface(&mut self, cx: &Caller, id: u64) -> Result<(), Fault> {
        if surface_by_id(&self.surfaces, id).is_some_and(|surface| surface.owner != cx.sender) {
            // Only the owner may destroy its own surface (issue #176: any
            // caller that guessed the id could close another app's window).
            return Err(refuse(errno::EACCES));
        }
        self.forget_surface(id);
        self.repaint_full();
        Ok(())
    }

    /// Begin a compositor-mediated drag from the owner's surface.
    fn drag_start(&mut self, cx: &Caller, id: u64, token: u64, mime: String) -> Result<(), Fault> {
        if self.drag_session.is_some() {
            return Err(refuse(errno::EBUSY));
        }
        let Some(surface) = surface_by_id(&self.surfaces, id) else {
            return Err(refuse(errno::EINVAL));
        };
        // Only the surface's own client may drag from it, and only with a
        // pointer button held: the gesture is what makes it a drag.
        if surface.owner != cx.sender {
            return Err(refuse(errno::EACCES));
        }
        if token == 0 || mime.is_empty() || mime.len() > display::MAX_MIME || !self.button_down {
            return Err(refuse(errno::EINVAL));
        }
        self.drag_begin(id, token, mime);
        Ok(())
    }

    /// The source's client abandons its drag.
    fn drag_cancel(&mut self, cx: &Caller, id: u64) -> Result<(), Fault> {
        let owns = self
            .drag_session
            .as_ref()
            .is_some_and(|active| active.source == id)
            && surface_by_id(&self.surfaces, id).is_some_and(|s| s.owner == cx.sender);
        if owns {
            self.drag_cancel();
        }
        Ok(())
    }

    /// Shell-only: one row per surface, bottom first: the desktop, the
    /// windows in z-order, then the panels.
    fn list_surfaces(&mut self, cx: &Caller) -> Result<Vec<wire::SurfaceRow>, Fault> {
        self.require_shell(cx)?;
        let layer = |keep: fn(&Surface) -> bool| self.surfaces.iter().filter(move |s| keep(s));
        Ok(layer(Surface::is_desktop)
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
            .collect())
    }

    /// The shell's work area, else the whole screen (which is how the shell
    /// learns the screen size before it sets one).
    fn get_work_area(&mut self, _: &Caller) -> Result<wire::GetWorkAreaReply, Fault> {
        let area = self.work_area();
        Ok(wire::GetWorkAreaReply {
            x: area.x,
            y: area.y,
            w: area.w,
            h: area.h,
        })
    }

    fn subscribe(&mut self, cx: &Caller, role: String, events: u64) -> Result<(), Fault> {
        Compositor::subscribe(self, cx, role, events)
    }

    fn get_theme(&mut self, _: &Caller) -> Result<wire::GetThemeReply, Fault> {
        Ok(get_theme())
    }

    fn set_title(&mut self, cx: &Caller, id: u64, title: String) -> Result<(), Fault> {
        Compositor::set_title(self, cx, id, &title)
    }

    fn hint_open_origin(
        &mut self,
        cx: &Caller,
        surface: u64,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
    ) -> Result<(), Fault> {
        let args = wire::HintOpenOriginArgs {
            surface,
            x,
            y,
            w,
            h,
        };
        Compositor::hint_open_origin(self, cx, args)
    }

    fn set_size_hints(
        &mut self,
        cx: &Caller,
        surface: u64,
        min_w: u32,
        min_h: u32,
        max_w: u32,
        max_h: u32,
    ) -> Result<(), Fault> {
        let args = wire::SetSizeHintsArgs {
            surface,
            min_w,
            min_h,
            max_w,
            max_h,
        };
        Compositor::set_size_hints(self, cx, args)
    }

    fn request_size(
        &mut self,
        cx: &Caller,
        surface: u64,
        width: u32,
        height: u32,
    ) -> Result<(), Fault> {
        let args = wire::RequestSizeArgs {
            surface,
            width,
            height,
        };
        Compositor::request_size(self, cx, args)
    }

    /// Move a panel; only its creator may.
    fn place_surface(&mut self, cx: &Caller, id: u64, x: i32, y: i32) -> Result<(), Fault> {
        let full = self.full();
        let Some(surface) = self.surfaces.iter_mut().find(|s| s.id == id) else {
            return Err(refuse(errno::ENOENT));
        };
        if surface.owner != cx.sender {
            return Err(refuse(errno::EACCES));
        }
        if !surface.is_panel() {
            return Err(refuse(errno::EINVAL));
        }
        let before = surface.window();
        (surface.x, surface.y) = clamp_panel((surface.w, surface.h), (x, y), full);
        let after = surface.window();
        if after != before {
            self.notify_surface(id, wire::CHANGE_MOVED);
            self.repaint(before.union(after));
        }
        Ok(())
    }

    /// Shell-only: restore, raise and focus a window (a taskbar click).
    fn activate_surface(&mut self, cx: &Caller, id: u64) -> Result<(), Fault> {
        self.require_shell(cx)?;
        self.window_arg(id)?;
        self.restore_and_focus(id);
        self.repaint_full();
        Ok(())
    }

    /// Shell-only: exactly the title-bar minimize button.
    fn minimize_surface(&mut self, cx: &Caller, id: u64) -> Result<(), Fault> {
        self.require_shell(cx)?;
        self.window_arg(id)?;
        if surface_by_id(&self.surfaces, id).is_some_and(|surface| !surface.minimized) {
            Compositor::minimize_surface(self, id);
        }
        Ok(())
    }

    /// Shell-only: where windows may go (the screen minus the shell's
    /// taskbar); maximized windows are re-fitted to it.
    fn set_work_area(&mut self, cx: &Caller, x: i32, y: i32, w: i32, h: i32) -> Result<(), Fault> {
        self.require_shell(cx)?;
        let full = self.full();
        let Some(area) = self.on_screen(x, y, w.into(), h.into()) else {
            return Err(refuse(errno::EINVAL));
        };
        if self.work_area() != area {
            self.work = (area != full).then_some(area);
            self.reflow_maximized();
            self.repaint_full();
        }
        Ok(())
    }

    /// Shell-only: where a window's taskbar entry is, for the minimize and
    /// restore zoom; an empty rectangle forgets it.
    fn set_icon_geometry(
        &mut self,
        cx: &Caller,
        id: u64,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    ) -> Result<(), Fault> {
        self.require_shell(cx)?;
        let icon = self.on_screen(x, y, w.into(), h.into());
        let Some(surface) = self.surfaces.iter_mut().find(|s| s.id == id) else {
            return Err(refuse(errno::ENOENT));
        };
        surface.icon = icon;
        Ok(())
    }

    /// Shell-only: the next window any task creates soon zooms open from
    /// this rectangle (a start-menu row, a desktop icon).
    fn hint_launch_origin(
        &mut self,
        cx: &Caller,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
    ) -> Result<(), Fault> {
        self.require_shell(cx)?;
        if let Some(from) = self.on_screen(x, y, w.into(), h.into()) {
            self.launch_hint = Some(OpenHint {
                owner: cx.sender,
                from,
                expires: sys::clock() + HINT_TTL_TICKS,
            });
        }
        Ok(())
    }

    /// The screen in physical pixels, the UI scale (docs/hidpi-plan.md) and
    /// the refresh rate clients pace their presents to (`refresh.rs`). Open
    /// to every client: nothing here is private.
    fn get_output(&mut self, _: &Caller) -> Result<wire::GetOutputReply, Fault> {
        Ok(wire::GetOutputReply {
            width: self.screen.width() as u32,
            height: self.screen.height() as u32,
            scale: super::theme::scale() as u32,
            refresh_millihz: self.refresh.millihz(),
        })
    }

    /// This compositor has no tray flyouts: `EINVAL`, which callers ignore.
    fn allow_popup(
        &mut self,
        _: &Caller,
        _: u64,
        _: u64,
        _: i32,
        _: i32,
        _: u32,
        _: u32,
    ) -> Result<(), Fault> {
        Err(refuse(errno::EINVAL))
    }

    /// Only the shell's own task may take the keyboard.
    fn grab_panel_keys(&mut self, cx: &Caller, grab: bool) -> Result<(), Fault> {
        let is_shell = self
            .shell
            .as_ref()
            .is_some_and(|shell| shell.task == cx.sender && !shell.dead);
        if !is_shell {
            return Err(refuse(errno::EACCES));
        }
        self.set_panel_keys(grab);
        Ok(())
    }

    /// Keep (`ask`) or drop (default) the window when it is closed.
    fn set_close_mode(&mut self, cx: &Caller, id: u64, ask: bool) -> Result<(), Fault> {
        let Some(surface) = surface_by_id(&self.surfaces, id) else {
            return Err(refuse(errno::ENOENT));
        };
        if surface.owner != cx.sender {
            return Err(refuse(errno::EACCES));
        }
        if !surface.is_window() {
            return Err(refuse(errno::EINVAL));
        }
        if let Some(surface) = self.surfaces.iter_mut().find(|s| s.id == id) {
            surface.ask_close = ask;
        }
        Ok(())
    }

    /// Shell-only: restore, raise and focus every window created under
    /// package label `label` in `session` (paint order is kept, so the one
    /// that was topmost ends focused; a click on one of that app's toasts).
    /// The shell asks, the app never does, so a background app cannot take
    /// the focus by itself. The pair is what the kernel stamped on the
    /// creating `CreateSurface`, so it names the app whichever thread made
    /// the window; an unlabelled program is never named.
    fn activate_app(&mut self, cx: &Caller, label: u32, session: u64) -> Result<(), Fault> {
        self.require_shell(cx)?;
        if label == 0 {
            return Err(refuse(errno::EINVAL));
        }
        let windows = app_windows(&self.surfaces, label, session);
        if windows.is_empty() {
            return Err(refuse(errno::ENOENT));
        }
        for id in windows {
            self.restore_and_focus(id);
        }
        self.repaint_full();
        Ok(())
    }

    /// `Present` is one-way: no reply, only `BufferRelease` and `FrameDone`.
    fn present(&mut self, cx: &Caller, surface: u64, slot: u32, seq: u64, damage: Vec<wire::Rect>) {
        let args = wire::PresentArgs {
            surface,
            slot,
            seq,
            damage,
        };
        Compositor::present(self, cx.sender, args);
    }

    // The events travel from `xuid` to a client; one sent to it is ignored.
    fn pointer_move(&mut self, _: &Caller, _: i32, _: i32) {}
    fn pointer_down(&mut self, _: &Caller, _: i32, _: i32, _: u32) {}
    fn pointer_up(&mut self, _: &Caller, _: i32, _: i32, _: u32) {}
    fn key_down(&mut self, _: &Caller, _: u32) {}
    fn key_up(&mut self, _: &Caller, _: u32) {}
    fn window_close(&mut self, _: &Caller) {}
    fn drag_enter(&mut self, _: &Caller, _: i32, _: i32, _: String) {}
    fn drag_over(&mut self, _: &Caller, _: i32, _: i32) {}
    fn drag_leave(&mut self, _: &Caller) {}
    #[allow(clippy::too_many_arguments)]
    fn drop(
        &mut self,
        _: &Caller,
        _: i32,
        _: i32,
        _: u64,
        _: String,
        _: Option<u32>,
        _: Option<u64>,
    ) {
    }
    fn drag_ended(&mut self, _: &Caller, _: bool) {}
    #[allow(clippy::too_many_arguments)]
    fn surface_changed(
        &mut self,
        _: &Caller,
        _: u64,
        _: u32,
        _: i32,
        _: i32,
        _: i32,
        _: i32,
        _: bool,
        _: bool,
        _: Option<String>,
        _: u32,
        _: bool,
    ) {
    }
    fn focus_changed(&mut self, _: &Caller, _: Option<u64>) {}
    fn start_menu(&mut self, _: &Caller) {}
    fn buffer_release(&mut self, _: &Caller, _: u64, _: u32) {}
    fn frame_done(&mut self, _: &Caller, _: u64, _: u64) {}
    fn pointer_wheel(&mut self, _: &Caller, _: i32, _: i32, _: i32) {}
    fn configure(&mut self, _: &Caller, _: u64, _: u32, _: u32, _: u32) {}
    fn ping(&mut self, _: &Caller) {}
    fn dismiss(&mut self, _: &Caller) {}
    fn panel_key(&mut self, _: &Caller, _: u32) {}
    fn tray_keys(&mut self, _: &Caller) {}
}
