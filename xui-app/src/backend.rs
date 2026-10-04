//! The LazyOS display backend: an xui window that either owns the display
//! grant or rides `xuid` as a compositor client.
//!
//! Modelled on `xui-canvas`'s `OffscreenBackend` (a node table, painters
//! composited into a software [`Surface`]) with a real event loop:
//!
//! * **Owner mode** ([`LazyOSBackend::new`], the M0-M2 milestones): the kernel
//!   hands this task the whole framebuffer and its input stream, so [`run`]
//!   polls syscall 12, routes input, re-renders on invalidation, and presents
//!   through the grant. The window is the screen, drawn at
//!   `96 * uitheme::auto_scale(screen)` DPI (docs/hidpi-plan.md).
//! * **Client mode** ([`LazyOSBackend::new_client`], issue #168): the app
//!   resolves `xuid`, creates a surface through `os.lazy.display.v1`, attaches
//!   double-buffered pixel slots, presents damage rectangles, and receives
//!   pointer/key/close events on its event endpoint. The window manager (drag,
//!   minimize, taskbar, close) runs in `xuid`; closing the surface ends the
//!   loop.
//!
//! Keyboard routing (issue #151) is mode-independent: pointer presses move the
//! backend focus to the node under them (when it is focusable), `Tab` and
//! `Shift+Tab` cycle the focus across focus stops, `SetFocus`/`KillFocus` are
//! delivered to the affected widgets, and key/char events go to the focused
//! node rather than the node under the pointer. Modifier bits (and the client
//! key codes) are decoded per `docs/architecture/display.md`; `PageUp`/
//! `PageDown` reach the focused widget so the Editor can scroll.
//!
//! [`run`]: Backend::run

mod backdrop;
mod double_click;
mod event_loop;
mod focus;
mod geometry;
mod handlers;
mod input;
mod node;
mod origin;
mod pointer;
mod render;
mod session_input;
mod zorder;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use xui_canvas::{OffscreenBackend, Surface};
use xui_core::backend::{Painter, ParentRef, WidgetId, WindowId};
use xui_core::router::WidgetHost;
use xui_core::{Key, Modifiers, Rect};

use crate::client_window::{ClientState, ClientWindow, SurfaceRole};
use crate::display;
use crate::sys::{self, DisplayInfo};

/// The only DPI the bring-up supports (see `docs/xui-plan.md`, risks).
const DEFAULT_DPI: u32 = 96;
/// Input poll pacing in milliseconds; also the fastest repaint once something
/// is dirty.
const POLL_MILLIS: u64 = 5;
/// Largest batch of kernel input records drained per poll.
const INPUT_BATCH: usize = 64;
/// Longest a client-mode loop parks with no timer armed and no event
/// (docs/performance-plan.md P3.8): only a safety net, since every event
/// endpoint wakes the park and timers bound it.
const CLIENT_IDLE_TICKS: u64 = 100;
/// Receive buffer for one compositor event message.
const CLIENT_INPUT_BYTES: usize = 4096;

/// How a backend reaches the screen.
enum Mode {
    /// The task owns the display grant (syscall 12).
    Owner { display: DisplayInfo },
    /// The task is a `xuid` client over `os.lazy.display.v1`.
    Client(RefCell<ClientState>),
}

/// A bound display, an open window, and a node table.
pub struct LazyOSBackend {
    /// How this backend presents and receives input.
    mode: Mode,
    windows: RefCell<HashMap<u64, Window>>,
    nodes: RefCell<Vec<(WidgetId, Node)>>,
    next_window: Cell<u64>,
    next_widget: Cell<u64>,
    /// The window `run_app` opened; the event loop drives it.
    primary: Cell<Option<WindowId>>,
    /// Set whenever a node is invalidated; the loop repaints and clears it.
    dirty: Arc<AtomicBool>,
    /// Client mode: the rectangles invalidated since the last present, one per
    /// window: only they are repainted, copied and presented.
    damage: RefCell<HashMap<u64, Rect>>,
    /// Set by [`Backend::quit`].
    quit: Arc<AtomicBool>,
    /// Pointer position in window pixels, updated by move events; kernel button
    /// records carry no coordinates, so it is also used for presses.
    pointer: Cell<(i32, i32)>,
    /// The node the last in-window pointer move went to, told `MouseLeave`
    /// when the pointer leaves the window (a panel's last, outside move).
    hovered: Cell<Option<(WindowId, WidgetId)>>,
    /// The node keyboard events go to; set by [`Backend::focus`]. Keys target
    /// the focused node, not the node under the pointer.
    focused: Cell<Option<WidgetId>>,
    /// The node that captured the pointer ([`Backend::set_capture`]); it gets
    /// every move and release until released or destroyed.
    captured: Cell<Option<WidgetId>>,
    /// Recognizes the second press of a double-click (never borrowed across a
    /// delivery, so a widget may call back into the backend).
    clicks: RefCell<double_click::ClickTracker>,
    /// The modifier keys currently held, from the `Shift`/`Ctrl`/`Alt`/`Super`
    /// key records the kernel forwards to a bound compositor. Attached to every
    /// `KeyDown`/`KeyUp` so an app sees Ctrl+key chords.
    modifiers: Cell<Modifiers>,
    /// Keys an `inputd` session reported down and not yet up, so a focus loss
    /// can release them (`KeyboardLeave`).
    held_keys: RefCell<Vec<Key>>,
    /// Repeating timers armed by [`Backend::set_timer`], in PIT ticks.
    timers: RefCell<Vec<Timer>>,
    next_timer: Cell<usize>,
    /// Frames presented so far.
    frames: Cell<u64>,
    /// A one-shot callback run after the first frame reached the screen.
    on_first_frame: RefCell<Option<Box<dyn FnOnce()>>>,
    /// Content-size hints applied to every window this app opens, or `None`
    /// for fixed-size windows. Set before `run_app` with
    /// [`LazyOSBackend::set_size_hints`].
    size_hints: Cell<Option<(u32, u32, u32, u32)>>,
    /// The role of the next window `open_window` creates (client mode), reset
    /// to [`SurfaceRole::Window`] once used. Set with
    /// [`LazyOSBackend::set_next_role`].
    next_role: Cell<SurfaceRole>,
    /// The desktop's integer UI scale (docs/hidpi-plan.md): `GetOutput` from
    /// the compositor, or the automatic scale of the screen in owner mode.
    /// Every window runs at `96 * scale` DPI; sizes the app states in pixels
    /// (size hints, `window_size`) are design pixels, multiplied here.
    scale: Cell<u32>,
    /// Source of the shared text shaper. `xui-canvas` keeps its cosmic-text
    /// shaper crate-private, but the headless backend hands out that same
    /// `Send + Sync` shaper (cloning shares one font system, built on first use
    /// from the fonts the app registered), so a widget can measure text on a
    /// worker thread and this thread's canvas draws the resulting layouts. The
    /// backend is never opened or run; it only owns the shaper.
    shaper: OffscreenBackend,
}

/// One repeating timer; `deadline` is an absolute PIT tick (100 Hz).
struct Timer {
    id: usize,
    /// The window the timer belongs to, so its `Timer` event reaches that
    /// window and not whichever one the loop ticks first.
    window: u64,
    millis: u64,
    deadline: u64,
}

struct Window {
    /// The painting surface. Only the damaged rectangle of it is meaningful
    /// after a composite: painters run unclipped, so a node that straddles
    /// the damage also overdraws its neighbours outside it.
    surface: Surface,
    /// The composed frame (top-down RGBA, the window's size): the damaged
    /// rectangles of `surface`, accumulated. Presents copy from it.
    frame: Vec<u8>,
    sink: Option<Rc<dyn WidgetHost>>,
    /// The window's theme, for the background under every repaint.
    theme: xui_core::Theme,
    /// A picture drawn over the background, under every node (`backdrop`).
    backdrop: Option<Rc<xui_core::image::Image>>,
    dpi: u32,
    width: i32,
    height: i32,
    /// Client mode: this window's compositor surface, or `None` in owner mode.
    client: Option<ClientWindow>,
}

struct Node {
    window: WindowId,
    parent: ParentRef,
    bounds: Rect,
    visible: bool,
    enabled: bool,
    /// Whether this node takes part in pointer click-focus and the focus cycle.
    focus_stop: bool,
    /// Clips this node's descendants, in its own coordinates (`set_clip`).
    clip: Option<Rect>,
    text: String,
    painter: Option<Painter>,
}

impl LazyOSBackend {
    /// Owner mode: bind the display and install the bundled font.
    ///
    /// The font is registered before any text is measured or drawn, because
    /// the shaper builds its database once per thread.
    pub fn new() -> Result<LazyOSBackend, i64> {
        xui_canvas::set_default_font(crate::font::BYTES.to_vec());
        let mut display = DisplayInfo::default();
        sys::display_bind(&mut display)?;
        if display.width == 0 || display.height == 0 || display.va == 0 {
            let _ = sys::display_unbind();
            return Err(-ENOENT);
        }
        let backend = Self::with_mode(Mode::Owner { display });
        backend.set_scale(uitheme::auto_scale(
            display.width as u32,
            display.height as u32,
        ));
        Ok(backend)
    }

    /// Client mode: resolve the compositor ; the surface and its event
    /// channel is created in [`Backend::open_window`], once the app's
    /// window spec is known.
    pub fn new_client() -> Result<LazyOSBackend, i64> {
        xui_canvas::set_default_font(crate::font::BYTES.to_vec());
        let client = display::Client::connect()?;
        // A compositor that predates `GetOutput` draws at scale 1.
        let scale = client.get_output().map_or(1, |output| output.scale);
        let backend = Self::with_mode(Mode::Client(RefCell::new(ClientState::new(client))));
        backend.set_scale(scale);
        Ok(backend)
    }

    /// Fix the UI scale (clamped to what the desktop supports) and the
    /// pixel thresholds that follow it.
    fn set_scale(&self, scale: u32) {
        let scale = scale.clamp(1, uitheme::MAX_SCALE);
        self.scale.set(scale);
        crate::hidpi::set_layout_scale(scale as i32);
        *self.clicks.borrow_mut() = double_click::ClickTracker::scaled(scale as i32);
    }

    /// The desktop's integer UI scale (1 or 2).
    pub fn scale(&self) -> u32 {
        self.scale.get()
    }

    /// The DPI every window of this app runs at: `96 * scale`.
    pub fn dpi(&self) -> u32 {
        uitheme::dpi_for(self.scale.get())
    }

    /// A backend with the shared empty state and `mode`.
    fn with_mode(mode: Mode) -> LazyOSBackend {
        LazyOSBackend {
            mode,
            windows: RefCell::new(HashMap::new()),
            nodes: RefCell::new(Vec::new()),
            next_window: Cell::new(1),
            next_widget: Cell::new(1),
            primary: Cell::new(None),
            dirty: Arc::new(AtomicBool::new(false)),
            damage: RefCell::new(HashMap::new()),
            quit: Arc::new(AtomicBool::new(false)),
            pointer: Cell::new((0, 0)),
            hovered: Cell::new(None),
            focused: Cell::new(None),
            captured: Cell::new(None),
            clicks: RefCell::new(double_click::ClickTracker::new()),
            modifiers: Cell::new(Modifiers::NONE),
            held_keys: RefCell::new(Vec::new()),
            timers: RefCell::new(Vec::new()),
            next_timer: Cell::new(1),
            frames: Cell::new(0),
            on_first_frame: RefCell::new(None),
            size_hints: Cell::new(None),
            next_role: Cell::new(SurfaceRole::Window),
            scale: Cell::new(1),
            shaper: OffscreenBackend::new(),
        }
    }

    /// Make every window this app opens resizable within the given content
    /// bounds (`min_w`/`min_h` at least, `max_w`/`max_h` at most; a `max` of 0
    /// means the screen), in design pixels: they are multiplied by the UI
    /// scale. Call it before `run_app`. Apps that never call it
    /// keep the old fixed-size behaviour.
    pub fn set_size_hints(&self, min_w: u32, min_h: u32, max_w: u32, max_h: u32) {
        self.size_hints.set(Some((min_w, min_h, max_w, max_h)));
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

    /// Whether this backend is a compositor client.
    pub fn is_client(&self) -> bool {
        matches!(self.mode, Mode::Client(_))
    }

    /// The window size in pixels, for sizing the app's window.
    ///
    /// A client's surface size is decided per window in [`Backend::open_window`]
    /// (and [`LazyOSBackend::window_size`](crate::launch::LazyOSBackend::window_size)
    /// reports the app's preferred size before that), so before any window opens
    /// a client reports `(0, 0)` rather than a stale screen size.
    pub fn screen(&self) -> (i32, i32) {
        match &self.mode {
            Mode::Owner { display } => (display.width as i32, display.height as i32),
            Mode::Client(_) => (0, 0),
        }
    }

    /// Registers a callback run once, after the first frame was presented
    /// (through the display grant or to `xuid`).
    pub fn on_first_frame(&self, callback: impl FnOnce() + 'static) {
        *self.on_first_frame.borrow_mut() = Some(Box::new(callback));
    }

    /// The node holding the keyboard focus, if any: lets an app's shortcut
    /// mapper (`Ui::on_key`) claim a key for one field only (LazyWeb's
    /// address bar takes Enter; the page keeps it for its forms).
    pub fn focused(&self) -> Option<WidgetId> {
        self.focused.get()
    }

    /// Frames presented through the display grant, or to `xuid`.
    pub fn frames(&self) -> u64 {
        self.frames.get()
    }

    /// Release the display (owner mode); the kernel mux repaints. A client
    /// destroys its surface: several clients share one compositor (#215).
    pub fn unbind(&self) {
        if matches!(self.mode, Mode::Owner { .. }) {
            let _ = sys::display_unbind();
        } else {
            self.destroy_surface();
        }
    }

    /// Destroy every client-mode surface and its event channel, if any.
    pub(super) fn destroy_surface(&self) {
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

/// The Linux-style positive errno a failed bind reports; the kernel's own
/// failure codes are negative and reach the user as the syscall result.
const ENOENT: i64 = 2;

#[cfg(test)]
mod test_support {
    use std::rc::Rc;

    use xui_core::router::WidgetHost;

    use super::*;
    use crate::client_window::ClientState;
    use crate::display::Client;

    /// A client-mode backend with no display behind it.
    pub(super) fn client_backend() -> LazyOSBackend {
        LazyOSBackend::with_mode(Mode::Client(RefCell::new(ClientState::new(
            Client::detached(),
        ))))
    }

    impl Window {
        /// A small surface-less window whose events go to `sink`.
        pub(super) fn for_tests(sink: Rc<dyn WidgetHost>) -> Window {
            Window {
                surface: Surface::new(64, 64),
                frame: Vec::new(),
                sink: Some(sink),
                theme: xui_core::Theme::light(),
                backdrop: None,
                dpi: DEFAULT_DPI,
                width: 64,
                height: 64,
                client: None,
            }
        }
    }
}
