//! The calls that talk to the compositor about this app's windows (client
//! mode): size hints and size requests, surface roles and ids, the desktop
//! theme, and tearing the surfaces down. In owner mode they do nothing.

use super::{LazyOSBackend, Mode};
use crate::client_window::SurfaceRole;
use crate::display;

impl LazyOSBackend {
    /// Make this app's windows resizable within the given content bounds
    /// (`min_w`/`min_h` at least, `max_w`/`max_h` at most; a `max` of 0 means
    /// the screen), in design pixels: they are multiplied by the UI scale.
    /// Windows already open take them at once, so an app may call it while it
    /// builds its first window (`launch::run`'s `make`). Apps that never call
    /// it keep the old fixed-size behaviour.
    pub fn set_size_hints(&self, min_w: u32, min_h: u32, max_w: u32, max_h: u32) {
        self.size_hints.set(Some((min_w, min_h, max_w, max_h)));
        for window in self.windows.borrow().values() {
            if let (true, Some(client)) = (window.resizable, &window.client) {
                self.send_size_hints(client.surface);
            }
        }
    }

    /// Sends the app's size hints, if any, for the compositor surface
    /// `surface` (client mode).
    pub(crate) fn send_size_hints(&self, surface: u64) {
        let (Some((min_w, min_h, max_w, max_h)), Mode::Client(state)) =
            (self.size_hints.get(), &self.mode)
        else {
            return;
        };
        // Design pixels to screen pixels; a `max` of 0 stays "the screen".
        let scale = self.scale();
        let _ = state.borrow().client.set_size_hints(
            surface,
            min_w * scale,
            min_h * scale,
            max_w * scale,
            max_h * scale,
        );
    }

    /// Make the *next* window this app opens a `role` surface (the shell's
    /// desktop and panels); later windows are ordinary windows again. A
    /// desktop or panel has no chrome, no size hints and no keyboard session,
    /// and a panel is placed at its `(x, y)` before its first frame.
    pub fn set_next_role(&self, role: SurfaceRole) {
        self.next_role.set(role);
    }

    /// The compositor connection, in client mode, for the protocol calls the
    /// toolkit has no words for (the shell's taskbar and work-area calls).
    pub fn display_client(&self) -> Option<display::Client> {
        match &self.mode {
            Mode::Client(state) => Some(state.borrow().client),
            Mode::Owner { .. } => None,
        }
    }

    /// The compositor surface id behind `window`, in client mode.
    pub fn surface_of(&self, window: xui_core::backend::WindowId) -> Option<u64> {
        let windows = self.windows.borrow();
        Some(windows.get(&window.raw())?.client.as_ref()?.surface)
    }

    /// Whether compositor surface `surface` is one of this app's windows.
    pub fn owns_surface(&self, surface: u64) -> bool {
        self.windows
            .borrow()
            .values()
            .any(|window| window.client.as_ref().is_some_and(|c| c.surface == surface))
    }

    /// The pointer's last position in window pixels, as the most recent
    /// pointer event reported it.
    pub fn pointer(&self) -> (i32, i32) {
        self.pointer.get()
    }

    /// Ask the compositor to resize this app's window to `width` x `height`
    /// content design pixels (a compact/expanded toggle; multiplied by the UI
    /// scale like the size hints). The compositor clamps it to
    /// the size hints and answers with a `Configure`, which reaches the app as
    /// `Event::Resize`; a failure (no hints, old compositor, owner mode) is
    /// ignored because the window then simply keeps its size.
    pub fn request_size(&self, width: u32, height: u32) {
        let Mode::Client(state) = &self.mode else {
            return;
        };
        let client = state.borrow().client;
        let (width, height) = (width * self.scale(), height * self.scale());
        for window in self.windows.borrow().values() {
            if let Some(surface) = &window.client {
                let _ = client.request_size(surface.surface, width, height);
            }
        }
    }

    /// The desktop's widget theme (`GetTheme` mode and accent) as an xui
    /// [`Theme`](xui_core::Theme); `None` in owner mode (no compositor) or
    /// when the compositor does not answer, and the app keeps xui's default.
    pub fn desktop_theme(&self) -> Option<xui_core::Theme> {
        let Mode::Client(state) = &self.mode else {
            return None;
        };
        let client = state.borrow().client;
        let reply = client.get_theme().ok()?;
        // A compositor that predates the fields sends neither: keep the default.
        let mode = uitheme::Mode::parse(&reply.mode)?;
        Some(xui_settings::theme_ops::xui_theme(mode, reply.accent))
    }

    /// Destroy every client-mode surface and its event channel, if any.
    pub(crate) fn destroy_surface(&self) {
        if let Mode::Client(state) = &self.mode {
            let client = state.borrow().client;
            for window in self.windows.borrow().values() {
                if let Some(surface) = &window.client {
                    surface.close(client);
                }
            }
        }
    }
}
