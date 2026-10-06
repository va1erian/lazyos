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
//!   pointer/key/close events on its event endpoint. `xuid` runs the window
//!   manager (drag, minimize, taskbar, close); closing the surface ends the run.
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
mod background;
mod compositor;
mod dnd;
mod double_click;
mod event_loop;
mod focus;
mod geometry;
mod handlers;
mod input;
mod moves;
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
use xui_core::backend::{Painter, ParentRef, TimerId, WidgetId, WindowId};
use xui_core::router::WidgetHost;
use xui_core::{Key, Modifiers, Rect};

use crate::client_window::{ClientState, ClientWindow, SurfaceRole};
use crate::display;
use crate::sys::{self, DisplayInfo};

pub use dnd::{DragOffer, DropEvent};

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
const CLIENT_IDLE_NS: u64 = 1_000_000_000;
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
    /// Repeating timers armed by [`Backend::set_timer`].
    timers: RefCell<Vec<Timer>>,
    next_timer: Cell<usize>,
    /// The descriptor [`LazyOSBackend::watch_fd`] parks on, and whether it
    /// became readable since the primary window last heard of it.
    watched_fd: Cell<Option<i32>>,
    fd_ready: Cell<bool>,
    /// The window rectangle being repainted while painters run
    /// ([`LazyOSBackend::paint_damage`]).
    paint_damage: Cell<Option<Rect>>,
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
    /// Drag and drop: the app's hooks and the press that may become a drag.
    dnd: dnd::Dnd,
}

/// One repeating timer, on the monotonic nanosecond clock
/// ([`sys::monotonic_ns`]): a 16 ms timer fires every 16 ms, not at the
/// next 10 ms tick after it.
struct Timer {
    id: usize,
    /// The window the timer belongs to, so its `Timer` event reaches that
    /// window and not whichever one the loop ticks first.
    window: u64,
    period_ns: u64,
    deadline_ns: u64,
}

/// The `TimerId` of the `Timer` event that reports a readable watched
/// descriptor ([`LazyOSBackend::watch_fd`]); real timers count up from 1.
pub const FD_TIMER: TimerId = TimerId(usize::MAX);

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
    /// The background and backdrop, rendered once for the window's size.
    background: background::Background,
    dpi: u32,
    width: i32,
    height: i32,
    /// Client mode: this window's compositor surface, or `None` in owner mode.
    client: Option<ClientWindow>,
    /// Whether the window is an ordinary app window, which takes the app's
    /// size hints (the shell's desktop and panels are fixed-size).
    resizable: bool,
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
            watched_fd: Cell::new(None),
            fd_ready: Cell::new(false),
            paint_damage: Cell::new(None),
            frames: Cell::new(0),
            on_first_frame: RefCell::new(None),
            size_hints: Cell::new(None),
            next_role: Cell::new(SurfaceRole::Window),
            scale: Cell::new(1),
            shaper: OffscreenBackend::new(),
            dnd: dnd::Dnd::default(),
        }
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
                background: Default::default(),
                dpi: DEFAULT_DPI,
                width: 64,
                height: 64,
                client: None,
                resizable: true,
            }
        }
    }
}
