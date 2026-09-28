#![forbid(unsafe_code)]

//! An offscreen [`Backend`] that composites the portable widgets into a
//! software [`Surface`] instead of a window.
//!
//! It runs on every platform with no UI toolkit, so the same widget code that
//! the Win32 backend hosts can be rendered and snapshot-tested here (including
//! on headless CI runners). The windowing shell builds on the same surface.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use xui_core::backend::{
    Backend, Event, ImplKind, NodeKind, NodeSpec, Painter, ParentRef, PlatformSpec,
    Result as BackendResult, TextMetrics, TextStyle, TimerId, Waker, WidgetId, WindowId,
};
use xui_core::router::WidgetHost;
use xui_core::{Rect, Theme};

use crate::{RgbaImage, Surface};

/// The default dots-per-inch a surface is rendered at.
const DEFAULT_DPI: u32 = 96;

struct OffscreenWindow {
    surface: Surface,
    sink: Option<Rc<dyn WidgetHost>>,
    background: xui_core::Color,
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

/// A backend that renders into a software surface.
pub struct OffscreenBackend {
    windows: RefCell<HashMap<u64, OffscreenWindow>>,
    nodes: RefCell<Vec<(WidgetId, Node)>>,
    next_window: std::cell::Cell<u64>,
    next_widget: std::cell::Cell<u64>,
}

impl Default for OffscreenBackend {
    fn default() -> OffscreenBackend {
        OffscreenBackend::new()
    }
}

impl OffscreenBackend {
    /// A backend with no windows.
    pub fn new() -> OffscreenBackend {
        OffscreenBackend {
            windows: RefCell::new(HashMap::new()),
            nodes: RefCell::new(Vec::new()),
            next_window: std::cell::Cell::new(1),
            next_widget: std::cell::Cell::new(1),
        }
    }

    fn allocate(cell: &std::cell::Cell<u64>) -> u64 {
        let id = cell.get();
        cell.set(id + 1);
        id
    }

    /// Opens a window rendered at `dpi`, so a test can exercise high-DPI
    /// layouts. [`Backend::open_window`] uses [`DEFAULT_DPI`].
    pub fn open_window_at(&self, spec: &PlatformSpec, dpi: u32) -> BackendResult<WindowId> {
        let id = WindowId::from_raw(Self::allocate(&self.next_window));
        let width = spec.width.to_px(dpi).value().max(1) as u32;
        let height = spec.height.to_px(dpi).value().max(1) as u32;
        self.windows.borrow_mut().insert(
            id.raw(),
            OffscreenWindow {
                surface: Surface::new(width, height),
                sink: None,
                background: Theme::light().background,
                dpi,
                width: width as i32,
                height: height as i32,
            },
        );
        Ok(id)
    }

    /// Renders `window`'s visible nodes, in creation order, into an image.
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

    /// Delivers `event` to the topmost node under its position, as a window
    /// system would, returning whether it was consumed.
    pub fn inject(&self, window: WindowId, event: Event) -> bool {
        let Some((x, y)) = event.position() else {
            return false;
        };
        let target = self
            .nodes
            .borrow()
            .iter()
            .rev()
            .find(|(_, node)| {
                node.window == window
                    && node.visible
                    && node.enabled
                    && node.bounds.contains(xui_core::Point::new(x, y))
            })
            .map(|(id, _)| *id);
        let Some(target) = target else {
            return false;
        };
        let sink = self
            .windows
            .borrow()
            .get(&window.raw())
            .and_then(|entry| entry.sink.clone());
        sink.is_some_and(|sink| sink.deliver(target, &event))
    }

    /// The dots-per-inch `window` is rendered at.
    pub fn dpi_of(&self, window: WindowId) -> u32 {
        self.windows
            .borrow()
            .get(&window.raw())
            .map_or(DEFAULT_DPI, |entry| entry.dpi)
    }

    fn with_node<R>(&self, id: WidgetId, f: impl FnOnce(&mut Node) -> R) -> Option<R> {
        self.nodes
            .borrow_mut()
            .iter_mut()
            .find(|(node_id, _)| *node_id == id)
            .map(|(_, node)| f(node))
    }
}

impl Backend for OffscreenBackend {
    fn run(&self) -> i32 {
        0
    }

    fn quit(&self, _code: i32) {}

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
        self.open_window_at(spec, DEFAULT_DPI)
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

    fn invalidate(&self, _id: WidgetId) {}

    fn invalidate_rect(&self, _id: WidgetId, _rect: Rect) {}

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
        crate::text::measure(text, style, dpi, i32::MAX)
    }

    fn dpi(&self, window: WindowId) -> u32 {
        self.dpi_of(window)
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
