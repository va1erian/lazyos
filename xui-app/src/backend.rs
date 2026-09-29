//! The LazyOS display backend: an xui window that either owns the display
//! grant or rides `xuid` as a compositor client.
//!
//! Modelled on `xui-canvas`'s `OffscreenBackend` (a node table, painters
//! composited into a software [`Surface`]) with a real event loop:
//!
//! * **Owner mode** ([`LazyOSBackend::new`], the M0-M2 milestones): the kernel
//!   hands this task the whole framebuffer and its input stream, so [`run`]
//!   polls syscall 12, routes input, re-renders on invalidation, and presents
//!   through the grant. DPI is fixed at 96 and the window is the screen.
//! * **Client mode** ([`LazyOSBackend::new_client`], issue #168): the app
//!   resolves `xuid`, creates a surface through `os.lazy.display.v1`, attaches
//!   a shared pixel buffer, commits damage rectangles, and receives
//!   pointer/key/close events on its event endpoint. The window manager (drag,
//!   minimize, taskbar, close) runs in `xuid`; closing the surface ends the
//!   loop.
//!
//! Keyboard routing (issue #151) is mode-independent: pointer presses move the
//! backend focus to the node under them (when it is focusable), `Tab` (owner
//! mode) and `PageUp`/`PageDown` (both modes; `xuid` reserves `Tab` for
//! surface focus) cycle the focus across focus stops, `SetFocus`/`KillFocus`
//! are delivered to the affected widgets, and key/char events go to the
//! focused node rather than the node under the pointer.
//!
//! [`run`]: Backend::run

mod event_loop;
mod focus;
mod handlers;
mod input;
mod node;
mod render;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use xui_canvas::Surface;
use xui_core::backend::{Painter, ParentRef, WidgetId, WindowId};
use xui_core::router::WidgetHost;
use xui_core::{Color, Rect};

use crate::client_window::ClientState;
use crate::display;
use crate::sys::{self, DisplayInfo};

/// The only DPI the bring-up supports (see `docs/xui-plan.md`, risks).
const DEFAULT_DPI: u32 = 96;
/// Input poll pacing in milliseconds; also the fastest repaint once something
/// is dirty.
const POLL_MILLIS: u64 = 5;
/// Largest batch of kernel input records drained per poll.
const INPUT_BATCH: usize = 64;
/// Longest the client-mode event receive parks before the loop runs timers.
const CLIENT_POLL_TICKS: u64 = 1;
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
    /// Client mode: the rectangles invalidated since the last commit.
    damage: Cell<Option<Rect>>,
    /// Set by [`Backend::quit`].
    quit: Arc<AtomicBool>,
    /// Pointer position in window pixels, updated by move events; kernel button
    /// records carry no coordinates, so it is also used for presses.
    pointer: Cell<(i32, i32)>,
    /// Client mode: the last screen-absolute pointer position, used to recover
    /// the surface origin (see [`LazyOSBackend::route_client_event`]).
    last_abs: Cell<Option<(i32, i32)>>,
    /// Client mode: the surface origin derived from the last press.
    origin: Cell<Option<(i32, i32)>>,
    /// The node keyboard events go to; set by [`Backend::focus`]. Keys target
    /// the focused node, not the node under the pointer.
    focused: Cell<Option<WidgetId>>,
    /// Repeating timers armed by [`Backend::set_timer`], in PIT ticks.
    timers: RefCell<Vec<Timer>>,
    next_timer: Cell<usize>,
    /// Frames presented so far.
    frames: Cell<u64>,
    /// A one-shot callback run after the first frame reached the screen.
    on_first_frame: RefCell<Option<Box<dyn FnOnce()>>>,
}

/// One repeating timer; `deadline` is an absolute PIT tick (100 Hz).
struct Timer {
    id: usize,
    millis: u64,
    deadline: u64,
}

struct Window {
    surface: Surface,
    sink: Option<Rc<dyn WidgetHost>>,
    background: Color,
    dpi: u32,
    width: i32,
    height: i32,
}

struct Node {
    window: WindowId,
    parent: ParentRef,
    bounds: Rect,
    visible: bool,
    enabled: bool,
    /// Whether this node takes part in pointer click-focus and the focus cycle.
    focus_stop: bool,
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
        Ok(Self::with_mode(Mode::Owner { display }))
    }

    /// Client mode: resolve the compositor ; the surface and its event
    /// channel is created in [`Backend::open_window`], once the app's
    /// window spec is known.
    pub fn new_client() -> Result<LazyOSBackend, i64> {
        xui_canvas::set_default_font(crate::font::BYTES.to_vec());
        let client = display::Client::connect()?;
        Ok(Self::with_mode(Mode::Client(RefCell::new(
            ClientState::new(client),
        ))))
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
            damage: Cell::new(None),
            quit: Arc::new(AtomicBool::new(false)),
            pointer: Cell::new((0, 0)),
            last_abs: Cell::new(None),
            origin: Cell::new(None),
            focused: Cell::new(None),
            timers: RefCell::new(Vec::new()),
            next_timer: Cell::new(1),
            frames: Cell::new(0),
            on_first_frame: RefCell::new(None),
        }
    }

    /// Whether this backend is a compositor client.
    pub fn is_client(&self) -> bool {
        matches!(self.mode, Mode::Client(_))
    }

    /// The window size in pixels, for sizing the app's window.
    pub fn screen(&self) -> (i32, i32) {
        match &self.mode {
            Mode::Owner { display } => (display.width as i32, display.height as i32),
            Mode::Client(state) => state.borrow().rect,
        }
    }

    /// Registers a callback run once, after the first present (owner mode) or
    /// the first commit (client mode).
    pub fn on_first_frame(&self, callback: impl FnOnce() + 'static) {
        *self.on_first_frame.borrow_mut() = Some(Box::new(callback));
    }

    /// Frames presented through the display grant, or committed to `xuid`.
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

    /// Destroy the client-mode surface and its event channel, if any.
    pub(super) fn destroy_surface(&self) {
        if let Mode::Client(state) = &self.mode {
            state.borrow_mut().close_surface();
        }
    }
}

/// The Linux-style positive errno a failed bind reports; the kernel's own
/// failure codes are negative and reach the user as the syscall result.
const ENOENT: i64 = 2;
