//! The `xui_core::backend::Backend` trait implementation for
//! [`LazyOSBackend`].

use std::rc::Rc;
use std::sync::atomic::Ordering;

use xui_canvas::Surface;
use xui_core::backend::{
    Backend, BackendError, ImplKind, NodeKind, NodeSpec, Painter, ParentRef, PlatformSpec,
    Result as BackendResult, TextMetrics, TextStyle, TimerId, Waker, WidgetId, WindowId,
};
use xui_core::router::WidgetHost;
use xui_core::{Rect, Theme};

use crate::sys;

use super::{LazyOSBackend, Mode, Node, Timer, Window, DEFAULT_DPI, POLL_MILLIS};

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
