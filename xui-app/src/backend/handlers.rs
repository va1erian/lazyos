//! The `xui_core::backend::Backend` trait implementation for
//! [`LazyOSBackend`].

use std::rc::Rc;
use std::sync::atomic::Ordering;

use xui_canvas::Surface;
use xui_core::backend::{
    Backend, BackendError, Event, ImplKind, NodeKind, NodeSpec, Painter, ParentRef, PlatformSpec,
    Result as BackendResult, TextMetrics, TextStyle, TimerId, Waker, WidgetId, WindowId,
};
use xui_core::router::WidgetHost;
use xui_core::{Rect, Theme};

use crate::client_window::{ClientWindow, SurfaceRole};
use crate::sys;

use super::geometry::absolute_bounds;
use super::zorder::family;
use super::{LazyOSBackend, Mode, Node, Timer, Window, DEFAULT_DPI, POLL_MILLIS};

impl Backend for LazyOSBackend {
    fn run(&self) -> i32 {
        let Some(primary) = self.primary.get() else {
            return 1;
        };
        self.present(primary);
        while !self.quit.load(Ordering::Relaxed) {
            // Snapshot the open windows: an app may open or close one (the
            // Files explorer opens a window per folder) while a tick runs.
            let windows: Vec<u64> = self.windows.borrow().keys().copied().collect();
            if windows.is_empty() {
                break;
            }
            for raw in windows {
                self.tick(xui_core::backend::WindowId::from_raw(raw));
                if self.quit.load(Ordering::Relaxed) {
                    break;
                }
            }
            if self.quit.load(Ordering::Relaxed) {
                break;
            }
            if self.is_client() {
                // Each client window's event receive already parked this task
                // for up to one tick; no extra sleep.
                continue;
            }
            sys::sleep_millis(POLL_MILLIS);
        }
        0
    }

    fn quit(&self, _code: i32) {
        self.quit.store(true, Ordering::Relaxed);
    }

    fn text_shaper(&self) -> Box<dyn xui_core::backend::TextShaper> {
        self.shaper.text_shaper()
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
        let dpi = self.dpi();
        let width = spec.width.to_px(dpi).value().max(1) as u32;
        let height = spec.height.to_px(dpi).value().max(1) as u32;
        // A client opens one surface per window, so the Files explorer's
        // one-window-per-folder model works; the connection is shared.
        // The role applies to this window only (see `set_next_role`).
        let role = self.next_role.take();
        let client = match &self.mode {
            Mode::Client(state) => Some(
                ClientWindow::open(state.borrow().client, width, height, &spec.title, role)
                    .map_err(BackendError::Other)?,
            ),
            Mode::Owner { .. } => None,
        };
        // The compositor may have resized the surface before the first attach;
        // the painting surface must match the buffer that was attached.
        let (width, height) = client.as_ref().map_or((width, height), |window| {
            (window.rect.0 as u32, window.rect.1 as u32)
        });
        // Declare the window resizable (if the app opted in) right after
        // `CreateSurface`, before any input can reach it. The desktop and
        // panels are chromeless and fixed-size.
        let hints = self
            .size_hints
            .get()
            .filter(|_| role == SurfaceRole::Window);
        if let (Some((min_w, min_h, max_w, max_h)), Some(surface), Mode::Client(state)) =
            (hints, client.as_ref(), &self.mode)
        {
            // Design pixels to screen pixels; a `max` of 0 stays "the screen".
            let scale = self.scale();
            let (min_w, min_h, max_w, max_h) =
                (min_w * scale, min_h * scale, max_w * scale, max_h * scale);
            let _ =
                state
                    .borrow()
                    .client
                    .set_size_hints(surface.surface, min_w, min_h, max_w, max_h);
        }
        self.windows.borrow_mut().insert(
            id.raw(),
            Window {
                surface: Surface::new(width, height),
                frame: Vec::new(),
                sink: None,
                theme: Theme::light(),
                dpi,
                width: width as i32,
                height: height as i32,
                client,
            },
        );
        if self.is_client() {
            // A new surface must be committed once even if nothing draws on it
            // before its first tick.
            self.add_damage(id, Rect::new(0, 0, width as i32, height as i32));
        }
        if self.primary.get().is_none() {
            self.primary.set(Some(id));
        }
        Ok(id)
    }

    fn close_window(&self, window: WindowId) {
        let removed = self.windows.borrow_mut().remove(&window.raw());
        if let (Some(surface), Mode::Client(state)) = (removed.and_then(|w| w.client), &self.mode) {
            surface.close(state.borrow().client);
        }
        self.nodes
            .borrow_mut()
            .retain(|(_, node)| node.window != window);
        // Drop any pending damage and timers for the closed window.
        self.damage.borrow_mut().remove(&window.raw());
        self.timers
            .borrow_mut()
            .retain(|timer| timer.window != window.raw());
        if self.primary.get() == Some(window) {
            self.primary.set(None);
        }
        self.focused.set(None);
        self.captured.set(None);
        self.clicks.borrow_mut().forget_window(window);
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
                clip: None,
                text: spec.text.clone(),
                painter: None,
            },
        ));
        // A new node has to reach the screen even when nothing else repaints:
        // an app that swaps one screen for another builds the new widgets in
        // one update, and the compositor only refreshes what was damaged.
        self.dirty.store(true, Ordering::Relaxed);
        if self.is_client() && spec.visible {
            if let Some((window, bounds)) = self.absolute_damage(id) {
                self.add_damage(window, bounds);
            }
        }
        Ok(id)
    }

    fn destroy(&self, id: WidgetId) {
        // The pixels the node covered must be repainted by whatever is left
        // underneath (an app swapping one screen for another relies on it), so
        // its damage is recorded before the node is forgotten.
        let damage = if self.is_client() {
            self.absolute_damage(id)
        } else {
            None
        };
        self.dirty.store(true, Ordering::Relaxed);
        if let Some((window, bounds)) = damage {
            self.add_damage(window, bounds);
        }
        let mut nodes = self.nodes.borrow_mut();
        nodes.retain(|(node_id, _)| *node_id != id);
        if self.focused.get() == Some(id) {
            self.focused.set(None);
        }
        self.clicks.borrow_mut().forget_widget(id);
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
        // A capture or pending click on a node the cascade removed is stale.
        if let Some(captured) = self.captured.get() {
            if !nodes.iter().any(|(node_id, _)| *node_id == captured) {
                self.captured.set(None);
            }
        }
        if let Some(focused) = self.focused.get() {
            if !nodes.iter().any(|(node_id, _)| *node_id == focused) {
                self.focused.set(None);
            }
        }
        self.clicks
            .borrow_mut()
            .forget_unless(|target| nodes.iter().any(|(node_id, _)| *node_id == target));
    }

    fn apply_moves(&self, window: WindowId, moves: &[(WidgetId, Rect)]) {
        let mut nodes = self.nodes.borrow_mut();
        for (id, rect) in moves {
            let Some(index) = nodes.iter().position(|(node_id, _)| node_id == id) else {
                continue;
            };
            // Damage is window-absolute: the old and the new position of the
            // node and of every descendant (a child may extend past its parent).
            let family = self.is_client().then(|| family(&nodes, *id));
            let before: Vec<Rect> = family
                .iter()
                .flatten()
                .filter_map(|member| absolute_bounds(&nodes, *member))
                .collect();
            nodes[index].1.bounds = *rect;
            let after = family
                .iter()
                .flatten()
                .filter_map(|member| absolute_bounds(&nodes, *member));
            for area in before.into_iter().chain(after) {
                self.add_damage(window, area);
            }
        }
    }

    fn set_visible(&self, id: WidgetId, visible: bool) {
        if self.is_client() {
            if let Some((window, bounds)) = self.absolute_damage(id) {
                self.add_damage(window, bounds);
            }
        }
        self.with_node(id, |node| node.visible = visible);
    }

    fn raise(&self, id: WidgetId) {
        self.raise_node(id);
    }

    fn set_enabled(&self, id: WidgetId, enabled: bool) {
        self.with_node(id, |node| node.enabled = enabled);
        self.damage_node(id);
    }

    fn set_clip(&self, id: WidgetId, rect: Option<Rect>) {
        self.with_node(id, |node| node.clip = rect);
    }

    fn set_capture(&self, id: WidgetId) {
        self.captured.set(Some(id));
    }

    fn release_capture(&self) {
        let Some(id) = self.captured.take() else {
            return;
        };
        // Tell the node after the capture is cleared, so its handler may
        // capture again or call back into the backend.
        if let Some(window) = self.window_of(id) {
            self.deliver(window, id, &Event::CaptureChanged);
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

    fn invalidate(&self, id: WidgetId) {
        self.damage_node(id);
    }

    fn invalidate_rect(&self, id: WidgetId, _rect: Rect) {
        self.damage_node(id);
    }

    fn set_painter(&self, id: WidgetId, painter: Painter) {
        self.with_node(id, |node| node.painter = Some(painter));
        self.damage_node(id);
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

    fn set_window_title(&self, window: WindowId, title: &str) {
        // Owner mode draws no chrome. In client mode the compositor renames the
        // window; a refusal (an older `xuid`) leaves the creation title.
        let Mode::Client(state) = &self.mode else {
            return;
        };
        let client = state.borrow().client;
        let surface = {
            let mut windows = self.windows.borrow_mut();
            windows
                .get_mut(&window.raw())
                .and_then(|entry| entry.client.as_mut())
                .filter(|c| c.title != title)
                .map(|c| {
                    c.title = title.to_owned();
                    c.surface
                })
        };
        if let Some(surface) = surface {
            let _ = client.set_title(surface, title);
        }
    }

    fn set_theme(&self, window: WindowId, theme: &Theme) {
        let full = {
            let mut windows = self.windows.borrow_mut();
            let Some(entry) = windows.get_mut(&window.raw()) else {
                return;
            };
            entry.theme = *theme;
            Rect::new(0, 0, entry.width, entry.height)
        };
        // Only damaged pixels are repainted, and the background is under all
        // of them.
        if self.is_client() {
            self.add_damage(window, full);
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    fn set_timer(&self, window: WindowId, millis: u32) -> TimerId {
        let id = self.next_timer.get();
        self.next_timer.set(id + 1);
        let millis = (millis as u64).max(1);
        let deadline = sys::clock_ticks().saturating_add(millis.div_ceil(10));
        self.timers.borrow_mut().push(Timer {
            id,
            window: window.raw(),
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

    /// The session clipboard, via `clipboardd` (issue #115), so Copy in one app
    /// and Paste in another share text. `clipboardd`'s absence falls back to an
    /// in-process store, and the call never waits for the service, so a console
    /// image is unaffected.
    fn clipboard_text(&self) -> Option<String> {
        crate::platform::clipboard::text()
    }

    fn set_clipboard_text(&self, text: &str) {
        crate::platform::clipboard::set_text(text);
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
