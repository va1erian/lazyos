#![forbid(unsafe_code)]

//! The [`Backend`] contract over the offscreen compositor.

use std::rc::Rc;

use xui_core::backend::{
    Backend, BackendError, Event, FontSpec, ImplKind, NodeKind, NodeSpec, Painter, ParentRef,
    PlatformSpec, Result as BackendResult, TextLayout, TextMetrics, TextShaper, TextStyle, TimerId,
    Waker, WidgetId, WindowId,
};
use xui_core::image::Image;
use xui_core::router::WidgetHost;
use xui_core::{Rect, Theme};

use crate::text_layout::CosmicShaper;

use super::{Node, OffscreenBackend};

impl Backend for OffscreenBackend {
    fn run(&self) -> i32 {
        let hook = self.run_hook.borrow_mut().take();
        if let Some(hook) = hook {
            hook();
        }
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
        self.open_window_at(spec, self.dpi)
    }

    fn close_window(&self, window: WindowId) {
        self.windows.borrow_mut().remove(&window.raw());
        self.nodes
            .borrow_mut()
            .retain(|(_, node)| node.window != window);
    }

    fn set_window_icon(&self, window: WindowId, icon: &Image) {
        if let Some(entry) = self.windows.borrow_mut().get_mut(&window.raw()) {
            entry.icon = Some(icon.clone());
        }
    }

    fn capture(&self, window: WindowId) -> BackendResult<Image> {
        let image = self
            .render(window)
            .ok_or_else(|| BackendError::Other("no such window".into()))?;
        Image::from_rgba(image.width, image.height, image.pixels)
            .map_err(|error| BackendError::Other(error.to_string()))
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
        let id = WidgetId::from_raw(OffscreenBackend::allocate(&self.next_widget));
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
                clip: None,
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
        drop(nodes);
        self.forget_focus_if_gone();
    }

    fn apply_moves(&self, window: WindowId, moves: &[(WidgetId, Rect)]) {
        let mut resized = Vec::new();
        {
            let mut nodes = self.nodes.borrow_mut();
            for (id, rect) in moves {
                if let Some((_, node)) = nodes.iter_mut().find(|(node_id, _)| node_id == id) {
                    if node.bounds.size() != rect.size() {
                        resized.push((*id, *rect));
                    }
                    node.bounds = *rect;
                }
            }
        }
        // A moved node gets no other size notification here (there is no HWND
        // to fire WM_SIZE on), so a resized widget must be told directly or its
        // own cached layout (e.g. a scrollbar's track) goes stale.
        let sink = self
            .windows
            .borrow()
            .get(&window.raw())
            .and_then(|entry| entry.sink.clone());
        if let Some(sink) = sink {
            for (id, rect) in resized {
                sink.deliver(
                    id,
                    &Event::Resize {
                        width: rect.width(),
                        height: rect.height(),
                    },
                );
            }
        }
    }

    fn set_visible(&self, id: WidgetId, visible: bool) {
        self.with_node(id, |node| node.visible = visible);
    }

    fn set_enabled(&self, id: WidgetId, enabled: bool) {
        self.with_node(id, |node| node.enabled = enabled);
    }

    fn set_clip(&self, id: WidgetId, rect: Option<Rect>) {
        self.with_node(id, |node| node.clip = rect);
    }

    fn set_capture(&self, id: WidgetId) {
        *self.captured.borrow_mut() = Some(id);
    }

    fn release_capture(&self) {
        let previous = self.captured.borrow_mut().take();
        let Some(id) = previous else {
            return;
        };
        let window = self
            .nodes
            .borrow()
            .iter()
            .find(|(node_id, _)| *node_id == id)
            .map(|(_, node)| node.window);
        let Some(window) = window else {
            return;
        };
        let sink = self
            .windows
            .borrow()
            .get(&window.raw())
            .and_then(|entry| entry.sink.clone());
        if let Some(sink) = sink {
            sink.deliver(id, &Event::CaptureChanged);
        }
    }

    fn raise(&self, id: WidgetId) {
        let mut nodes = self.nodes.borrow_mut();
        if let Some(at) = nodes.iter().position(|(node_id, _)| *node_id == id) {
            let entry = nodes.remove(at);
            nodes.push(entry);
        }
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

    fn text_shaper(&self) -> Box<dyn TextShaper> {
        Box::new(self.text.get_or_init(CosmicShaper::new).clone())
    }

    fn layout_text(
        &self,
        text: &str,
        spec: &FontSpec,
        max_width: f32,
        dpi: u32,
    ) -> Box<dyn TextLayout> {
        self.text
            .get_or_init(CosmicShaper::new)
            .layout(text, spec, max_width, dpi)
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
            entry.theme = *theme;
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
