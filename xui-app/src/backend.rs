//! The LazyOS display backend: a single window that owns the display grant.
//!
//! Modelled on `xui-canvas`'s `OffscreenBackend` (a node table, painters
//! composited into a software [`Surface`]) with a real event loop: the kernel
//! hands this task the whole framebuffer and its input stream, so
//! [`Backend::run`] polls input, routes it to the widget under the pointer,
//! re-renders on invalidation, and presents through syscall 12. DPI is fixed
//! at 96 and the window is the screen; multi-window sessions arrive with the
//! compositor protocol client (see `docs/xui-plan.md` M3).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use xui_canvas::{RgbaImage, Surface};
use xui_core::backend::{
    Backend, Event, ImplKind, NodeKind, NodeSpec, Painter, ParentRef, PlatformSpec,
    Result as BackendResult, TextMetrics, TextStyle, TimerId, Waker, WidgetId, WindowId,
};
use xui_core::router::WidgetHost;
use xui_core::{Color, Key, Modifiers, MouseButton, Point, Rect, Theme};

use crate::sys::{self, button, event, key, DisplayInfo, EVENT_BYTES};

/// The only DPI the bring-up supports (see `docs/xui-plan.md`, risks).
const DEFAULT_DPI: u32 = 96;
/// Input poll pacing in milliseconds; also the fastest repaint once something
/// is dirty.
const POLL_MILLIS: u64 = 5;
/// Largest batch of kernel input records drained per poll.
const INPUT_BATCH: usize = 64;

/// A bound display, an open window, and a node table.
pub struct LazyOSBackend {
    /// Geometry and mapping of the screen buffer from [`sys::display_bind`].
    display: DisplayInfo,
    windows: RefCell<HashMap<u64, Window>>,
    nodes: RefCell<Vec<(WidgetId, Node)>>,
    next_window: Cell<u64>,
    next_widget: Cell<u64>,
    /// The window `run_app` opened; the event loop drives it.
    primary: Cell<Option<WindowId>>,
    /// Set whenever a node is invalidated; the loop repaints and clears it.
    dirty: Arc<AtomicBool>,
    /// Set by [`Backend::quit`].
    quit: Arc<AtomicBool>,
    /// Pointer position in screen pixels, seeded by the kernel at bind and
    /// updated by move events; button records carry no coordinates.
    pointer: Cell<(i32, i32)>,
    /// Frames presented so far.
    frames: Cell<u64>,
    /// A one-shot callback run after the first frame reached the screen.
    on_first_frame: RefCell<Option<Box<dyn FnOnce()>>>,
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
    text: String,
    painter: Option<Painter>,
}

impl LazyOSBackend {
    /// Bind the display and install the bundled font.
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
        Ok(LazyOSBackend {
            display,
            windows: RefCell::new(HashMap::new()),
            nodes: RefCell::new(Vec::new()),
            next_window: Cell::new(1),
            next_widget: Cell::new(1),
            primary: Cell::new(None),
            dirty: Arc::new(AtomicBool::new(false)),
            quit: Arc::new(AtomicBool::new(false)),
            pointer: Cell::new((0, 0)),
            frames: Cell::new(0),
            on_first_frame: RefCell::new(None),
        })
    }

    /// The screen size in pixels, for sizing the app's window.
    pub fn screen(&self) -> (i32, i32) {
        (self.display.width as i32, self.display.height as i32)
    }

    /// Registers a callback run once, after the first present.
    pub fn on_first_frame(&self, callback: impl FnOnce() + 'static) {
        *self.on_first_frame.borrow_mut() = Some(Box::new(callback));
    }

    /// Frames presented through the display grant.
    pub fn frames(&self) -> u64 {
        self.frames.get()
    }

    /// Release the display; the kernel mux repaints.
    pub fn unbind(&self) {
        let _ = sys::display_unbind();
    }

    fn allocate(cell: &Cell<u64>) -> u64 {
        let id = cell.get();
        cell.set(id + 1);
        id
    }

    /// Composites `window`'s visible nodes, in creation order, into an image.
    pub fn render(&self, window: WindowId) -> Option<RgbaImage> {
        let mut windows = self.windows.borrow_mut();
        let entry = windows.get_mut(&window.raw())?;
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
        Some(entry.surface.to_image())
    }

    /// Render and blit the window through the display grant.
    fn present(&self, window: WindowId) -> bool {
        let Some(image) = self.render(window) else {
            return false;
        };
        let (width, height) = self.screen();
        if image.width as i32 != width || image.height as i32 != height {
            return false;
        }
        let size = self.display.size as usize;
        if image.pixels.len() != size {
            return false;
        }
        // Safety: `va`/`size` are the mapping the kernel installed for this
        // task's screen buffer at bind; `image.pixels` is exactly `size` bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(image.pixels.as_ptr(), self.display.va as *mut u8, size);
        }
        if sys::display_present(0, 0, width, height).is_err() {
            return false;
        }
        self.frames.set(self.frames.get() + 1);
        if self.frames.get() == 1 {
            if let Some(callback) = self.on_first_frame.borrow_mut().take() {
                callback();
            }
        }
        true
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

    /// Drain the kernel input queue, translate and route each record.
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
                let Some(event) = self.translate(raw) else {
                    continue;
                };
                let target = event
                    .position()
                    .and_then(|(x, y)| self.hit(window, x, y))
                    .unwrap_or(WidgetId::NONE);
                self.deliver(window, target, &event);
                if raw.kind == event::KEY_DOWN {
                    if let Some(character) = key_char(raw.a as u32) {
                        self.deliver(window, target, &Event::Char(character));
                    }
                }
            }
        }
    }

    /// Translate one kernel input record into an `xui` event; `None` for an
    /// unknown kind.
    fn translate(&self, raw: sys::RawEvent) -> Option<Event> {
        let (x, y) = self.pointer.get();
        match raw.kind {
            event::POINTER_MOVE => {
                self.pointer.set((raw.a, raw.b));
                Some(Event::MouseMove {
                    x: raw.a,
                    y: raw.b,
                    modifiers: Modifiers::NONE,
                })
            }
            event::POINTER_DOWN => Some(Event::MouseDown {
                x,
                y,
                button: mouse_button(raw.a),
                modifiers: Modifiers::NONE,
            }),
            event::POINTER_UP => Some(Event::MouseUp {
                x,
                y,
                button: mouse_button(raw.a),
                modifiers: Modifiers::NONE,
            }),
            event::KEY_DOWN => key_event(raw.a, true),
            event::KEY_UP => key_event(raw.a, false),
            _ => None,
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
            self.pump_input(window);
            // A wake drains the message queue; widget mappers enqueue while an
            // input record is being routed, so this runs after every batch.
            self.deliver(window, WidgetId::NONE, &Event::Wake);
            if self.dirty.swap(false, Ordering::Relaxed) {
                self.present(window);
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
                text: spec.text.clone(),
                painter: None,
            },
        ));
        Ok(id)
    }

    fn destroy(&self, id: WidgetId) {
        let mut nodes = self.nodes.borrow_mut();
        nodes.retain(|(node_id, _)| *node_id != id);
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
                node.bounds = *rect;
            }
        }
    }

    fn set_visible(&self, id: WidgetId, visible: bool) {
        self.with_node(id, |node| node.visible = visible);
    }

    fn set_enabled(&self, id: WidgetId, enabled: bool) {
        self.with_node(id, |node| node.enabled = enabled);
    }

    fn focus(&self, _id: WidgetId) {}

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

    fn invalidate(&self, _id: WidgetId) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    fn invalidate_rect(&self, _id: WidgetId, _rect: Rect) {
        self.dirty.store(true, Ordering::Relaxed);
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

    fn set_timer(&self, _window: WindowId, _millis: u32) -> TimerId {
        TimerId(0)
    }

    fn kill_timer(&self, _window: WindowId, _id: TimerId) {}

    fn supports(&self, _kind: NodeKind) -> ImplKind {
        ImplKind::Painted
    }
}

/// The Linux-style positive errno a failed bind reports; the kernel's own
/// failure codes are negative and reach the user as the syscall result.
const ENOENT: i64 = 2;

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
