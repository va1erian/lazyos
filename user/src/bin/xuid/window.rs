//! Window-management operations (issues #143, #175, #194 split): z-order,
//! focus, minimize/close, lookup and hit helpers, and per-surface event
//! forwarding, moved out of `xuid.rs` unchanged.

use alloc::string::String;
use alloc::vec::Vec;
use user::messenger::display::{self, wire, Rect};
use user::messenger::Endpoint;

use super::compositor::Compositor;
use super::surface::Surface;

/// Find a surface by id.
pub(super) fn surface_by_id(surfaces: &[Surface], id: u64) -> Option<&Surface> {
    surfaces.iter().find(|surface| surface.id == id)
}
/// Whether `rect` contains the point `(x, y)`.
pub(super) fn contains(rect: Rect, point: (i32, i32)) -> bool {
    point.0 >= rect.x && point.1 >= rect.y && point.0 < rect.x + rect.w && point.1 < rect.y + rect.h
}

/// The surface-relative pointer position inside a window's content.
pub(super) fn relative(surfaces: &[Surface], id: u64, point: (i32, i32)) -> (i32, i32) {
    match surfaces.iter().find(|surface| surface.id == id) {
        Some(surface) => {
            let content = surface.content();
            (point.0 - content.x, point.1 - content.y)
        }
        None => (0, 0),
    }
}

/// Move a surface to the tail of `surfaces`, i.e. the top of the paint order.
pub(super) fn raise(surfaces: &mut Vec<Surface>, id: u64) {
    if let Some(index) = surfaces.iter().position(|surface| surface.id == id) {
        if index + 1 != surfaces.len() {
            let surface = surfaces.remove(index);
            surfaces.push(surface);
        }
    }
}

/// The topmost visible window's id (the desktop and panels are never
/// focusable).
pub(super) fn topmost_visible(surfaces: &[Surface]) -> Option<u64> {
    surfaces
        .iter()
        .rev()
        .find(|surface| surface.is_window() && !surface.minimized)
        .map(|surface| surface.id)
}

/// Restore a window if minimized, raise it, and focus it.
pub(super) fn restore(surfaces: &mut Vec<Surface>, focused: &mut Option<u64>, id: u64) {
    if let Some(surface) = surfaces.iter_mut().find(|surface| surface.id == id) {
        surface.minimized = false;
    }
    raise(surfaces, id);
    *focused = Some(id);
}

/// Raise a newly created window to the top of the paint order and focus it,
/// returning whether the focused surface changed.
///
/// A window that just opened must come up in front and focused, or it opens
/// behind the window that spawned it with the old title bar still highlighted
/// (the Files app double-clicking a folder). This includes the first window,
/// which keeps the old "focus the only window" behaviour; focusing the surface
/// that is already focused reports no change, so the caller does not send a
/// redundant `FocusChanged`.
pub(super) fn focus_on_create(
    surfaces: &mut Vec<Surface>,
    focused: &mut Option<u64>,
    id: u64,
) -> bool {
    raise(surfaces, id);
    let changed = *focused != Some(id);
    *focused = Some(id);
    changed
}

impl Compositor {
    /// Minimize a surface, moving focus to the next visible surface.
    pub(super) fn minimize_surface(&mut self, id: u64) {
        // A window still opening lands first: it is hidden while it flies
        // in, which these state changes would misread as minimized.
        self.finish_opening();
        if let Some(surface) = self.surfaces.iter_mut().find(|surface| surface.id == id) {
            surface.minimized = true;
        }
        if self.focused == Some(id) {
            self.focused = topmost_visible(&self.surfaces);
            self.notify_focus();
        }
        self.iconify(id);
        self.wm_mark("MINIMIZE", id);
        // The row carries the post-minimize focus flag, so send it after the
        // focus recompute.
        self.notify_surface(id, wire::CHANGE_MINIMIZED);
        self.repaint_full();
    }

    /// Close a surface: tell the client through a one-way `WindowClose` event
    /// and drop it; the full-screen repaint lets the windows below show
    /// through.
    pub(super) fn close_surface(&mut self, id: u64) {
        if let Some(surface) = self.surfaces.iter().find(|surface| surface.id == id) {
            let _ = display::send_event(
                &Endpoint::from_raw(surface.events),
                &mut self.scratch,
                wire::METHOD_WINDOWCLOSE,
                Ok(Vec::new()),
            );
        }
        self.forget_surface(id);
        self.repaint_full();
    }

    /// Drop every trace of surface `id`: the title-bar drag, a drag & drop
    /// session it sources or hovers, its Alt+Tab entry, the shell's view of
    /// it and its focus. Shared by close and destroy so neither can leave a
    /// dangling id behind; the caller repaints.
    pub(super) fn forget_surface(&mut self, id: u64) {
        // A window-manager title-bar drag on the surface ends with it.
        if self.drag.is_some_and(|active| active.id == id) {
            self.drag = None;
        }
        // An interactive resize on the surface ends with it; its outline is
        // erased by the caller's repaint.
        if self.resize.is_some_and(|active| active.id == id) {
            self.resize = None;
        }
        // A pending double-click record for the surface would otherwise match a
        // later window that reused nothing (ids are never reused) but is still
        // stale state.
        if self
            .last_title_click
            .is_some_and(|(click_id, ..)| click_id == id)
        {
            self.last_title_click = None;
        }
        // A drag & drop session whose source or hovered target goes away ends
        // now.
        let stranding = self
            .drag_session
            .as_ref()
            .is_some_and(|active| active.source == id || active.target == Some(id));
        if stranding {
            self.drag_cancel();
        }
        if surface_by_id(&self.surfaces, id).is_some_and(|surface| surface.is_window()) {
            self.wm_mark("CLOSED", id);
        }
        self.notify_destroyed(id);
        if let Some(tab) = self.alt_tab.as_mut() {
            // The Alt+Tab snapshot may not outlive the surface.
            tab.order.retain(|&entry| entry != id);
            if tab.order.is_empty() {
                self.alt_tab = None;
            } else if tab.selected >= tab.order.len() {
                tab.selected = 0;
            }
        }
        self.forget_layer(id);
        remove_surface(&mut self.surfaces, id);
        if self.focused == Some(id) {
            self.focused = topmost_visible(&self.surfaces);
            self.notify_focus();
        }
    }
}

/// Drop surface `id` and close its transferred event endpoint, so repeated
/// create/destroy cycles cannot exhaust this task's handle table. Events
/// already queued (e.g. `WindowClose`) stay deliverable after the close.
pub(super) fn remove_surface(surfaces: &mut Vec<Surface>, id: u64) {
    if let Some(index) = surfaces.iter().position(|surface| surface.id == id) {
        let mut surface = surfaces.remove(index);
        surface.release_buffers();
        if surface.events != 0 {
            let _ = Endpoint::from_raw(surface.events).close();
        }
    }
}

/// Move focus to the next visible surface, wrapping around and skipping
/// minimized ones; the new focus is raised so its title bar is not covered.
pub(super) fn cycle_focus(surfaces: &mut Vec<Surface>, focused: &mut Option<u64>) {
    if surfaces
        .iter()
        .filter(|surface| surface.is_window())
        .all(|surface| surface.minimized)
    {
        *focused = None;
        return;
    }
    let current_id = *focused;
    if let Some(current) = current_id.and_then(|id| surfaces.iter().position(|s| s.id == id)) {
        for step in 1..=surfaces.len() {
            let index = (current + step) % surfaces.len();
            if surfaces[index].is_window()
                && !surfaces[index].minimized
                && Some(surfaces[index].id) != current_id
            {
                let id = surfaces[index].id;
                raise(surfaces, id);
                *focused = Some(id);
                return;
            }
        }
    }
    // No other visible surface: focus (and raise) the first visible one.
    if let Some(id) = surfaces
        .iter()
        .find(|surface| surface.is_window() && !surface.minimized)
        .map(|surface| surface.id)
    {
        raise(surfaces, id);
        *focused = Some(id);
    }
}
/// Send one event (`body` already encoded) to a surface's endpoint, ignoring
/// a closed peer.
pub(super) fn forward(
    surfaces: &[Surface],
    scratch: &mut Vec<u8>,
    id: Option<u64>,
    method: u32,
    body: Result<Vec<u8>, libmessenger::Error>,
) {
    let Some(surface) = id.and_then(|id| surfaces.iter().find(|surface| surface.id == id)) else {
        return;
    };
    let _ = display::send_event(&Endpoint::from_raw(surface.events), scratch, method, body);
}

impl Compositor {
    /// Send surface `id`'s client a one-way `Configure(width, height, state)`
    /// so it can attach a buffer of the new content size and repaint. Used by
    /// resize and maximize; the compositor keeps drawing the old buffer,
    /// cropped or padded, until the client catches up.
    pub(super) fn send_configure(
        &mut self,
        id: u64,
        events: u64,
        width: i32,
        height: i32,
        state: u32,
    ) {
        let args = wire::ConfigureArgs {
            surface: id,
            width: width.max(0) as u32,
            height: height.max(0) as u32,
            state,
        };
        let _ = display::send_event(
            &Endpoint::from_raw(events),
            &mut self.scratch,
            wire::METHOD_CONFIGURE,
            wire::encode_configure_args(&args),
        );
    }
}

/// A bare surface for the create-focus self-test: no buffers, no chrome.
pub(super) fn test_surface(id: u64, minimized: bool, desktop: bool) -> Surface {
    let role = if desktop {
        wire::ROLE_DESKTOP
    } else {
        wire::ROLE_WINDOW
    };
    Surface {
        id,
        title: String::new(),
        x: 0,
        y: 0,
        w: 10,
        h: 10,
        events: 0,
        owner: 0,
        pixels: 0,
        bytes: 0,
        buf_w: 0,
        buf_h: 0,
        hints: None,
        maximized: None,
        snap: None,
        slots: Default::default(),
        minimized,
        role,
        icon: None,
        input_session: false,
    }
}

/// Boot check of the create-time focus/raise rule documented on
/// [`focus_on_create`]: `XUID:FOCUS:PASS` or `XUID:FOCUS:FAIL`.
pub(super) fn selftest_focus_on_create() -> &'static str {
    let tail = |surfaces: &[Surface], id: u64| surfaces.last().is_some_and(|s| s.id == id);

    // The first window gets focus (the pre-existing behaviour).
    let mut surfaces = alloc::vec![test_surface(1, false, false)];
    let mut focused = None;
    let first = focus_on_create(&mut surfaces, &mut focused, 1) && focused == Some(1);

    // A second window opens on top and takes focus from the first.
    surfaces.push(test_surface(2, false, false));
    let second =
        focus_on_create(&mut surfaces, &mut focused, 2) && focused == Some(2) && tail(&surfaces, 2);

    // Re-focusing the window that is already focused reports no change, so the
    // caller sends no redundant `FocusChanged`.
    let again = !focus_on_create(&mut surfaces, &mut focused, 2) && focused == Some(2);

    // Focusing a window that is not at the tail raises it.
    let mut stack = alloc::vec![
        test_surface(10, false, false),
        test_surface(11, false, false),
        test_surface(12, false, false),
    ];
    let mut focus = None;
    let raised =
        focus_on_create(&mut stack, &mut focus, 10) && focus == Some(10) && tail(&stack, 10);

    // A window created while the previous focus is minimized takes focus and
    // leaves the minimized window minimized, below the new one.
    let mut minimized_stack = alloc::vec![test_surface(20, true, false)];
    let mut minimized_focus = None;
    focus_on_create(&mut minimized_stack, &mut minimized_focus, 20);
    minimized_stack.push(test_surface(21, false, false));
    let after_minimized = focus_on_create(&mut minimized_stack, &mut minimized_focus, 21)
        && minimized_focus == Some(21)
        && tail(&minimized_stack, 21)
        && minimized_stack.iter().any(|s| s.id == 20 && s.minimized);

    // A desktop at the tail of the vector is not a window: a window created
    // after it still ends up at the tail and focused.
    let mut desktop_stack = alloc::vec![
        test_surface(30, false, false),
        test_surface(31, false, true),
        test_surface(32, false, false),
    ];
    let mut desktop_focus = None;
    let after_desktop = focus_on_create(&mut desktop_stack, &mut desktop_focus, 32)
        && desktop_focus == Some(32)
        && tail(&desktop_stack, 32);

    if first && second && again && raised && after_minimized && after_desktop {
        "XUID:FOCUS:PASS\n"
    } else {
        "XUID:FOCUS:FAIL\n"
    }
}
