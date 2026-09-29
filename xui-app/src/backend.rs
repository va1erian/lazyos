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

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use xui_canvas::Surface;
use xui_core::backend::{
    Backend, BackendError, Event, ImplKind, NodeKind, NodeSpec, Painter, ParentRef, PlatformSpec,
    Result as BackendResult, TextMetrics, TextStyle, TimerId, Waker, WidgetId, WindowId,
};
use xui_core::router::WidgetHost;
use xui_core::{Color, Key, Modifiers, MouseButton, Point, Rect, Theme};

use crate::client_window::ClientState;
use crate::display::{self, EventKind};
use crate::sys::{self, button, errno, event, key, DisplayInfo, EVENT_BYTES};

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

    fn allocate(cell: &Cell<u64>) -> u64 {
        let id = cell.get();
        cell.set(id + 1);
        id
    }

    /// The window a node belongs to.
    fn window_of(&self, id: WidgetId) -> Option<WindowId> {
        self.nodes
            .borrow()
            .iter()
            .find(|(node_id, _)| *node_id == id)
            .map(|(_, node)| node.window)
    }

    /// Composites `window`'s visible nodes, in creation order, into its
    /// surface, ready for [`LazyOSBackend::present`] to copy.
    fn composite(&self, window: WindowId) -> bool {
        let mut windows = self.windows.borrow_mut();
        let Some(entry) = windows.get_mut(&window.raw()) else {
            return false;
        };
        entry.surface.fill(entry.background);
        let dpi = entry.dpi;
        let paints: Vec<(Rect, Painter)> = self
            .nodes
            .borrow()
            .iter()
            .filter(|(_, node)| node.window == window && node.visible)
            .filter_map(|(_, node)| node.painter.clone().map(|painter| (node.bounds, painter)))
            .collect();
        for (bounds, painter) in paints {
            entry
                .surface
                .with_canvas_at(bounds, dpi, |canvas| painter(canvas));
        }
        true
    }

    /// Render and blit the window: a damage-rectangle present through the
    /// display grant (owner mode), or a damage-rectangle commit to the
    /// compositor (client mode).
    fn present(&self, window: WindowId) -> bool {
        if !self.composite(window) {
            return false;
        }
        match &self.mode {
            Mode::Owner { display } => {
                let (width, height) = self.screen();
                let size = display.size as usize;
                let mut windows = self.windows.borrow_mut();
                let Some(entry) = windows.get_mut(&window.raw()) else {
                    return false;
                };
                let pixels = entry.surface.pixels();
                if pixels.len() != size {
                    return false;
                }
                // Safety: `va`/`size` are the mapping the kernel installed for
                // this task's screen buffer at bind; `pixels` is exactly
                // `size` bytes.
                unsafe {
                    core::ptr::copy_nonoverlapping(pixels.as_ptr(), display.va as *mut u8, size);
                }
                if sys::display_present(0, 0, width, height).is_err() {
                    return false;
                }
            }
            Mode::Client(state) => {
                let state = state.borrow();
                let full = Rect::new(0, 0, state.rect.0, state.rect.1);
                let damage = self.take_damage(full);
                let mut windows = self.windows.borrow_mut();
                let Some(entry) = windows.get_mut(&window.raw()) else {
                    return false;
                };
                let pixels = entry.surface.pixels();
                if state.va == 0 || pixels.len() > state.size as usize {
                    return false;
                }
                // Safety: `va`/`size` describe the shared buffer
                // `display_create_buffer` mapped into this task; `pixels` is
                // the window-sized RGBA image and fits inside it.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        pixels.as_ptr(),
                        state.va as *mut u8,
                        pixels.len(),
                    );
                }
                drop(windows);
                if state
                    .client
                    .commit(
                        state.surface,
                        (damage.left, damage.top, damage.width(), damage.height()),
                    )
                    .is_err()
                {
                    return false;
                }
            }
        }
        self.frames.set(self.frames.get() + 1);
        if self.frames.get() == 1 {
            if let Some(callback) = self.on_first_frame.borrow_mut().take() {
                callback();
            }
        }
        true
    }

    /// The damage accumulated since the last commit, clamped to `full`.
    fn take_damage(&self, full: Rect) -> Rect {
        let Some(damage) = self.damage.take() else {
            return full;
        };
        let clipped = Rect::new(
            damage.left.max(full.left),
            damage.top.max(full.top),
            damage.right.min(full.right),
            damage.bottom.min(full.bottom),
        );
        if clipped.is_empty() {
            full
        } else {
            clipped
        }
    }

    /// Grow the pending damage by `rect`.
    fn add_damage(&self, rect: Rect) {
        let merged = match self.damage.get() {
            Some(existing) => Rect::new(
                existing.left.min(rect.left),
                existing.top.min(rect.top),
                existing.right.max(rect.right),
                existing.bottom.max(rect.bottom),
            ),
            None => rect,
        };
        self.damage.set(Some(merged));
    }

    /// The topmost enabled, visible node under `(x, y)`.
    fn hit(&self, window: WindowId, x: i32, y: i32) -> Option<WidgetId> {
        self.nodes
            .borrow()
            .iter()
            .rev()
            .find(|(_, node)| {
                node.window == window
                    && node.visible
                    && node.enabled
                    && node.bounds.contains(Point::new(x, y))
            })
            .map(|(id, _)| *id)
    }

    /// Offer `event` to the sink installed by `run_app`.
    fn deliver(&self, window: WindowId, target: WidgetId, event: &Event) -> bool {
        let sink = self
            .windows
            .borrow()
            .get(&window.raw())
            .and_then(|entry| entry.sink.clone());
        sink.is_some_and(|sink| sink.deliver(target, event))
    }

    /// Give `id` the keyboard focus, notifying the widget that lost it.
    fn set_focus(&self, id: WidgetId) {
        let old = self.focused.replace(Some(id));
        if old == Some(id) {
            return;
        }
        let Some(window) = self.window_of(id) else {
            return;
        };
        if let Some(old) = old {
            if self.window_of(old) == Some(window) {
                self.deliver(window, old, &Event::KillFocus);
            }
        }
        self.deliver(window, id, &Event::SetFocus);
    }

    /// Whether `id` is a focus stop (a Tab-order control or a button-like one).
    fn is_focus_stop(&self, id: WidgetId) -> bool {
        self.nodes
            .borrow()
            .iter()
            .find(|(node_id, _)| *node_id == id)
            .is_some_and(|(_, node)| node.visible && node.enabled && node.focus_stop)
    }

    /// Move the keyboard focus to the next (or previous) focus stop.
    fn cycle_focus(&self, window: WindowId, forward: bool) {
        let stops: Vec<WidgetId> = self
            .nodes
            .borrow()
            .iter()
            .filter(|(_, node)| {
                node.window == window && node.visible && node.enabled && node.focus_stop
            })
            .map(|(id, _)| *id)
            .collect();
        if stops.is_empty() {
            return;
        }
        let current = self
            .focused
            .get()
            .and_then(|id| stops.iter().position(|stop| *stop == id));
        let next = match current {
            Some(index) if forward => (index + 1) % stops.len(),
            Some(index) => (index + stops.len() - 1) % stops.len(),
            None if forward => 0,
            None => stops.len() - 1,
        };
        self.set_focus(stops[next]);
    }

    /// Route one pointer move (window-relative coordinates).
    fn pointer_move(&self, window: WindowId, x: i32, y: i32) {
        self.pointer.set((x, y));
        let target = self.hit(window, x, y).unwrap_or(WidgetId::NONE);
        self.deliver(
            window,
            target,
            &Event::MouseMove {
                x,
                y,
                modifiers: Modifiers::NONE,
            },
        );
    }

    /// Route one pointer press: click-to-focus, then the press itself.
    fn pointer_down(&self, window: WindowId, x: i32, y: i32, button: MouseButton) {
        self.pointer.set((x, y));
        let target = self.hit(window, x, y);
        if let Some(id) = target {
            if self.is_focus_stop(id) {
                self.set_focus(id);
            }
        }
        self.deliver(
            window,
            target.unwrap_or(WidgetId::NONE),
            &Event::MouseDown {
                x,
                y,
                button,
                modifiers: Modifiers::NONE,
            },
        );
    }

    /// Route one pointer release.
    fn pointer_up(&self, window: WindowId, x: i32, y: i32, button: MouseButton) {
        self.pointer.set((x, y));
        let target = self.hit(window, x, y).unwrap_or(WidgetId::NONE);
        self.deliver(
            window,
            target,
            &Event::MouseUp {
                x,
                y,
                button,
                modifiers: Modifiers::NONE,
            },
        );
    }

    /// Route one key press: focus navigation first, then the focused widget.
    ///
    /// `Tab` is the canonical cycle key; `PageDown`/`PageUp` are accepted too
    /// because a compositor reserves `Tab` for surface focus and the kernel's
    /// PS/2 driver does not decode function keys (issue #168).
    fn key_down(&self, window: WindowId, code: u32) {
        match code {
            key::TAB | key::PAGE_DOWN => {
                self.cycle_focus(window, true);
            }
            key::PAGE_UP => {
                self.cycle_focus(window, false);
            }
            _ => {
                let target = self.focused.get().unwrap_or(WidgetId::NONE);
                if let Some(event) = key_event(code as i32, true) {
                    self.deliver(window, target, &event);
                }
                if let Some(character) = key_char(code) {
                    self.deliver(window, target, &Event::Char(character));
                }
            }
        }
    }

    /// Route one key release to the focused widget.
    fn key_up(&self, window: WindowId, code: u32) {
        let target = self.focused.get().unwrap_or(WidgetId::NONE);
        if let Some(event) = key_event(code as i32, false) {
            self.deliver(window, target, &event);
        }
    }

    /// Drain the kernel input queue (owner mode), translate and route records.
    fn pump_input(&self, window: WindowId) {
        let mut bytes = [0u8; EVENT_BYTES * INPUT_BATCH];
        while let Ok(count) = sys::display_input_poll(&mut bytes) {
            if count == 0 {
                break;
            }
            for index in 0..count {
                let Some(raw) = sys::decode_event(&bytes, index) else {
                    continue;
                };
                match raw.kind {
                    event::POINTER_MOVE => self.pointer_move(window, raw.a, raw.b),
                    event::POINTER_DOWN => {
                        let (x, y) = self.pointer.get();
                        self.pointer_down(window, x, y, mouse_button(raw.a));
                    }
                    event::POINTER_UP => {
                        let (x, y) = self.pointer.get();
                        self.pointer_up(window, x, y, mouse_button(raw.a));
                    }
                    event::KEY_DOWN => self.key_down(window, raw.a as u32),
                    event::KEY_UP => self.key_up(window, raw.a as u32),
                    _ => {}
                }
            }
        }
    }

    /// Drain the event endpoint (client mode): compositor messages carry
    /// pointer, key and window-close events.
    fn pump_client_input(&self, window: WindowId, events: u64) {
        let mut buf = [0u8; CLIENT_INPUT_BYTES];
        loop {
            let deadline = sys::clock_ticks().saturating_add(CLIENT_POLL_TICKS);
            match sys::msg_recv(events, &mut buf, deadline) {
                Ok(result) => {
                    let len = result.bytes as usize;
                    let Some(parcel) = display::decode_message(&buf[..len]) else {
                        continue;
                    };
                    if parcel.header.method == display::method::WINDOW_CLOSE {
                        self.quit.store(true, Ordering::Relaxed);
                        self.deliver(window, WidgetId::NONE, &Event::Close);
                        continue;
                    }
                    if let Some(event) = display::decode_event(&parcel) {
                        self.route_client_event(window, event);
                    }
                }
                Err(code) if code == -errno::ETIMEDOUT => break,
                Err(code) if code == -errno::EPIPE => {
                    // The compositor died; there is nothing to draw into.
                    self.quit.store(true, Ordering::Relaxed);
                    break;
                }
                Err(_) => break,
            }
        }
    }

    /// Route one decoded compositor event.
    ///
    /// `xuid` reports presses relative to the surface but moves in screen
    /// coordinates, so the surface origin is recovered from each press and
    /// applied to the moves that follow. Press events do not carry the button
    /// id (a protocol gap, noted in the PR), so they read as the left button.
    fn route_client_event(&self, window: WindowId, event: display::Event) {
        match event.kind {
            EventKind::PointerMove => {
                self.last_abs.set(Some((event.a as i32, event.b as i32)));
                if let Some((ox, oy)) = self.origin.get() {
                    self.pointer_move(window, event.a as i32 - ox, event.b as i32 - oy);
                }
            }
            EventKind::PointerDown => {
                let (x, y) = (event.a as i32, event.b as i32);
                if let Some((abs_x, abs_y)) = self.last_abs.get() {
                    self.origin.set(Some((abs_x - x, abs_y - y)));
                }
                self.pointer_down(window, x, y, MouseButton::Left);
            }
            EventKind::PointerUp => {
                self.pointer_up(window, event.a as i32, event.b as i32, MouseButton::Left);
            }
            EventKind::KeyDown => self.key_down(window, event.a as u32),
            EventKind::KeyUp => self.key_up(window, event.a as u32),
        }
    }

    /// Deliver every due timer's `Timer` event and re-arm it for its period.
    fn fire_timers(&self, window: WindowId) {
        let now = sys::clock_ticks();
        let mut due = Vec::new();
        for timer in self.timers.borrow_mut().iter_mut() {
            if now >= timer.deadline {
                due.push(timer.id);
                timer.deadline = now.saturating_add(timer.millis.div_ceil(10));
            }
        }
        for id in due {
            self.deliver(window, WidgetId::NONE, &Event::Timer { id: TimerId(id) });
        }
    }

    /// One event-loop iteration: drain input, flush widget messages, run due
    /// timers, and repaint when something is dirty.
    fn tick(&self, window: WindowId) {
        match &self.mode {
            Mode::Owner { .. } => self.pump_input(window),
            Mode::Client(state) => {
                let events = state.borrow().events;
                self.pump_client_input(window, events);
            }
        }
        // A wake drains the message queue; widget mappers enqueue while an
        // input record is being routed, so this runs after every batch.
        self.deliver(window, WidgetId::NONE, &Event::Wake);
        self.fire_timers(window);
        // Timer messages joined the queue after the wake above; drain them in
        // the same pass so a refresh paints without a poll-period delay.
        self.deliver(window, WidgetId::NONE, &Event::Wake);
        if self.dirty.swap(false, Ordering::Relaxed) {
            self.present(window);
        }
    }

    /// Destroy the client-mode surface and its event channel, if any.
    fn destroy_surface(&self) {
        if let Mode::Client(state) = &self.mode {
            state.borrow_mut().close_surface();
        }
    }

    fn with_node<R>(&self, id: WidgetId, f: impl FnOnce(&mut Node) -> R) -> Option<R> {
        self.nodes
            .borrow_mut()
            .iter_mut()
            .find(|(node_id, _)| *node_id == id)
            .map(|(_, node)| f(node))
    }
}

impl Backend for LazyOSBackend {
    fn run(&self) -> i32 {
        let Some(window) = self.primary.get() else {
            return 1;
        };
        self.present(window);
        while !self.quit.load(Ordering::Relaxed) {
            self.tick(window);
            if self.is_client() {
                // The client event receive already parked this task for up to
                // one tick; no extra sleep.
                continue;
            }
            if self.quit.load(Ordering::Relaxed) {
                break;
            }
            sys::sleep_millis(POLL_MILLIS);
        }
        0
    }

    fn quit(&self, _code: i32) {
        self.quit.store(true, Ordering::Relaxed);
    }

    fn wake(&self, _window: WindowId) {}

    fn waker(&self, _window: WindowId) -> Waker {
        Box::new(|| {})
    }

    fn set_event_sink(&self, window: WindowId, sink: Rc<dyn WidgetHost>) {
        if let Some(entry) = self.windows.borrow_mut().get_mut(&window.raw()) {
            entry.sink = Some(sink);
        }
    }

    fn open_window(&self, spec: &PlatformSpec) -> BackendResult<WindowId> {
        let id = WindowId::from_raw(Self::allocate(&self.next_window));
        let dpi = DEFAULT_DPI;
        let width = spec.width.to_px(dpi).value().max(1) as u32;
        let height = spec.height.to_px(dpi).value().max(1) as u32;
        if let Mode::Client(state) = &self.mode {
            let mut state = state.borrow_mut();
            state
                .open_surface(width, height, &spec.title)
                .map_err(BackendError::Other)?;
        }
        self.windows.borrow_mut().insert(
            id.raw(),
            Window {
                surface: Surface::new(width, height),
                sink: None,
                background: Theme::light().background,
                dpi,
                width: width as i32,
                height: height as i32,
            },
        );
        self.primary.set(Some(id));
        Ok(id)
    }

    fn close_window(&self, window: WindowId) {
        self.windows.borrow_mut().remove(&window.raw());
        self.nodes
            .borrow_mut()
            .retain(|(_, node)| node.window != window);
        self.focused.set(None);
        self.destroy_surface();
    }

    fn create(&self, parent: ParentRef, spec: &NodeSpec) -> BackendResult<WidgetId> {
        let window = match parent {
            ParentRef::Window(window) => window,
            ParentRef::Widget(widget) => self
                .nodes
                .borrow()
                .iter()
                .find(|(id, _)| *id == widget)
                .map(|(_, node)| node.window)
                .ok_or(xui_core::backend::BackendError::CreateFailed("parent node"))?,
        };
        if !self.windows.borrow().contains_key(&window.raw()) {
            return Err(xui_core::backend::BackendError::CreateFailed("window"));
        }
        let id = WidgetId::from_raw(Self::allocate(&self.next_widget));
        self.nodes.borrow_mut().push((
            id,
            Node {
                window,
                parent,
                bounds: spec.bounds,
                visible: spec.visible,
                enabled: spec.enabled,
                focus_stop: focus_stop(spec),
                text: spec.text.clone(),
                painter: None,
            },
        ));
        Ok(id)
    }

    fn destroy(&self, id: WidgetId) {
        let mut nodes = self.nodes.borrow_mut();
        nodes.retain(|(node_id, _)| *node_id != id);
        if self.focused.get() == Some(id) {
            self.focused.set(None);
        }
        // Cascade: drop any node whose parent chain no longer exists.
        loop {
            let gone: Vec<WidgetId> = nodes
                .iter()
                .filter(|(_, node)| match node.parent {
                    ParentRef::Window(window) => !self.windows.borrow().contains_key(&window.raw()),
                    ParentRef::Widget(parent) => !nodes.iter().any(|(id, _)| *id == parent),
                })
                .map(|(id, _)| *id)
                .collect();
            if gone.is_empty() {
                break;
            }
            nodes.retain(|(id, _)| !gone.contains(id));
        }
    }

    fn apply_moves(&self, _window: WindowId, moves: &[(WidgetId, Rect)]) {
        let mut nodes = self.nodes.borrow_mut();
        for (id, rect) in moves {
            if let Some((_, node)) = nodes.iter_mut().find(|(node_id, _)| node_id == id) {
                if self.is_client() {
                    self.add_damage(node.bounds);
                    self.add_damage(*rect);
                }
                node.bounds = *rect;
            }
        }
    }

    fn set_visible(&self, id: WidgetId, visible: bool) {
        if self.is_client() {
            if let Some(bounds) = self
                .nodes
                .borrow()
                .iter()
                .find(|(node_id, _)| *node_id == id)
                .map(|(_, node)| node.bounds)
            {
                self.add_damage(bounds);
            }
        }
        self.with_node(id, |node| node.visible = visible);
    }

    fn set_enabled(&self, id: WidgetId, enabled: bool) {
        self.with_node(id, |node| node.enabled = enabled);
    }

    fn focus(&self, id: WidgetId) {
        self.set_focus(id);
    }

    fn set_text(&self, id: WidgetId, text: &str) {
        self.with_node(id, |node| node.text = text.to_string());
    }

    fn text(&self, id: WidgetId) -> String {
        self.nodes
            .borrow()
            .iter()
            .find(|(node_id, _)| *node_id == id)
            .map(|(_, node)| node.text.clone())
            .unwrap_or_default()
    }

    fn invalidate(&self, id: WidgetId) {
        self.dirty.store(true, Ordering::Relaxed);
        if self.is_client() {
            if let Some(bounds) = self
                .nodes
                .borrow()
                .iter()
                .find(|(node_id, _)| *node_id == id)
                .map(|(_, node)| node.bounds)
            {
                self.add_damage(bounds);
            }
        }
    }

    fn invalidate_rect(&self, id: WidgetId, _rect: Rect) {
        self.dirty.store(true, Ordering::Relaxed);
        if self.is_client() {
            if let Some(bounds) = self
                .nodes
                .borrow()
                .iter()
                .find(|(node_id, _)| *node_id == id)
                .map(|(_, node)| node.bounds)
            {
                self.add_damage(bounds);
            }
        }
    }

    fn set_painter(&self, id: WidgetId, painter: Painter) {
        self.with_node(id, |node| node.painter = Some(painter));
    }

    fn bounds(&self, id: WidgetId) -> Rect {
        self.nodes
            .borrow()
            .iter()
            .find(|(node_id, _)| *node_id == id)
            .map_or(Rect::default(), |(_, node)| node.bounds)
    }

    fn measure_text(&self, text: &str, style: &TextStyle, dpi: u32) -> TextMetrics {
        xui_canvas::measure_text(text, style, dpi, i32::MAX)
    }

    fn dpi(&self, window: WindowId) -> u32 {
        self.windows
            .borrow()
            .get(&window.raw())
            .map_or(DEFAULT_DPI, |entry| entry.dpi)
    }

    fn client_rect(&self, window: WindowId) -> Rect {
        self.windows
            .borrow()
            .get(&window.raw())
            .map_or(Rect::default(), |entry| {
                Rect::new(0, 0, entry.width, entry.height)
            })
    }

    fn set_theme(&self, window: WindowId, theme: &Theme) {
        if let Some(entry) = self.windows.borrow_mut().get_mut(&window.raw()) {
            entry.background = theme.background;
        }
    }

    fn set_timer(&self, _window: WindowId, millis: u32) -> TimerId {
        let id = self.next_timer.get();
        self.next_timer.set(id + 1);
        let millis = (millis as u64).max(1);
        let deadline = sys::clock_ticks().saturating_add(millis.div_ceil(10));
        self.timers.borrow_mut().push(Timer {
            id,
            millis,
            deadline,
        });
        TimerId(id)
    }

    fn kill_timer(&self, _window: WindowId, id: TimerId) {
        self.timers.borrow_mut().retain(|timer| timer.id != id.0);
    }

    fn supports(&self, _kind: NodeKind) -> ImplKind {
        ImplKind::Painted
    }
}

/// The Linux-style positive errno a failed bind reports; the kernel's own
/// failure codes are negative and reach the user as the syscall result.
const ENOENT: i64 = 2;

/// Whether a node joins click-focus and the focus cycle: the explicit Tab
/// order, plus the button-like controls xui marks as focusable at the platform
/// layer but not as tab stops.
fn focus_stop(spec: &NodeSpec) -> bool {
    spec.tab_stop
        || matches!(
            spec.kind,
            NodeKind::Button
                | NodeKind::CheckBox
                | NodeKind::Radio
                | NodeKind::Slider
                | NodeKind::ListView
                | NodeKind::TreeView
                | NodeKind::Toolbar
                | NodeKind::Tabs
                | NodeKind::ComboBox
        )
}

fn mouse_button(code: i32) -> MouseButton {
    match code as u32 {
        button::RIGHT => MouseButton::Right,
        button::MIDDLE => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

/// One key record into the `KeyDown`/`KeyUp` vocabulary. A printable key also
/// produces a separate [`Event::Char`] (see [`key_char`]).
fn key_event(code: i32, down: bool) -> Option<Event> {
    if !down {
        return Some(Event::KeyUp {
            key: key_of(code as u32),
            modifiers: Modifiers::NONE,
            system: false,
        });
    }
    Some(Event::KeyDown {
        key: key_of(code as u32),
        modifiers: Modifiers::NONE,
        repeat: 1,
        system: false,
    })
}

/// Map a kernel key code onto the `xui` virtual-key vocabulary.
fn key_of(code: u32) -> Key {
    match code {
        key::ENTER => Key::RETURN,
        key::BACKSPACE => Key::BACK,
        key::TAB => Key::TAB,
        key::ESCAPE => Key::ESCAPE,
        key::SPACE => Key::SPACE,
        key::LEFT => Key::LEFT,
        key::RIGHT => Key::RIGHT,
        key::UP => Key::UP,
        key::DOWN => Key::DOWN,
        key::PAGE_UP => Key::PAGE_UP,
        key::PAGE_DOWN => Key::PAGE_DOWN,
        key::HOME => Key::HOME,
        key::END => Key::END,
        // The kernel reports letters lowercase; the virtual-key codes are
        // uppercase, matching the Windows ABI `xui` mirrors.
        other if (b'a' as u32..=b'z' as u32).contains(&other) => {
            Key::from_code((other as u8).to_ascii_uppercase() as u16)
        }
        other => Key::from_code(other as u16),
    }
}

/// The character a printable key carries; `None` for a non-printing key.
fn key_char(code: u32) -> Option<char> {
    match code {
        key::ENTER => Some('\n'),
        key::TAB => Some('\t'),
        key::BACKSPACE => Some('\u{8}'),
        other if (0x20..=0x7e).contains(&other) => char::from_u32(other),
        _ => None,
    }
}
