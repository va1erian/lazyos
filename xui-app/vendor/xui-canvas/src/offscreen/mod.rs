#![forbid(unsafe_code)]

//! An offscreen [`xui_core::backend::Backend`] that composites the portable
//! widgets into a software [`Surface`] instead of a window.
//!
//! It runs on every platform with no UI toolkit, so the same widget code that
//! the Win32 backend hosts can be rendered and snapshot-tested here (including
//! on headless CI runners). The windowing shell builds on the same surface.

use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use xui_core::backend::{
    Canvas as _, Event, Painter, ParentRef, PlatformSpec, Result as BackendResult, WidgetId,
    WindowId,
};
use xui_core::router::WidgetHost;
use xui_core::{Image, Rect, Theme};

use crate::text_layout::CosmicShaper;
use crate::{RgbaImage, Surface};

mod backend;
#[cfg(test)]
mod capture_tests;
mod geometry;
#[cfg(test)]
mod tests;

use crate::geometry::{absolute_bounds, ancestor_clip, effectively_visible, hit_bounds, intersect};
use geometry::translate;

/// The default dots-per-inch a surface is rendered at.
const DEFAULT_DPI: u32 = 96;

struct OffscreenWindow {
    surface: Surface,
    sink: Option<Rc<dyn WidgetHost>>,
    theme: Theme,
    dpi: u32,
    width: i32,
    height: i32,
    /// The last icon the app set with `Backend::set_window_icon`.
    icon: Option<Image>,
}

struct Node {
    window: WindowId,
    parent: ParentRef,
    bounds: Rect,
    visible: bool,
    enabled: bool,
    text: String,
    painter: Option<Painter>,
    /// A clip on this node's descendants, in the node's own coordinates.
    clip: Option<Rect>,
}

/// One node to draw in creation order: its painter, if any, and its clip.
struct Draw {
    bounds: Rect,
    clip: Option<Rect>,
    painter: Option<Painter>,
}

/// Clears a window's in-progress render flag when a frame finishes, however the
/// render returns.
struct RenderGuard<'a> {
    rendering: &'a RefCell<HashSet<u64>>,
    raw: u64,
}

impl Drop for RenderGuard<'_> {
    fn drop(&mut self) {
        self.rendering.borrow_mut().remove(&self.raw);
    }
}

/// Puts a window's real surface back after compositing, on every exit path
/// (including a panic in a painter), so a later frame cannot keep the 1x1
/// placeholder that stood in for it while painters ran.
struct SurfaceRestore<'a> {
    backend: &'a OffscreenBackend,
    raw: u64,
    surface: Option<Surface>,
}

impl Drop for SurfaceRestore<'_> {
    fn drop(&mut self) {
        if let Some(surface) = self.surface.take()
            && let Some(entry) = self.backend.windows.borrow_mut().get_mut(&self.raw)
        {
            entry.surface = surface;
        }
    }
}

/// A backend that renders into a software surface.
pub struct OffscreenBackend {
    windows: RefCell<HashMap<u64, OffscreenWindow>>,
    nodes: RefCell<Vec<(WidgetId, Node)>>,
    next_window: std::cell::Cell<u64>,
    next_widget: std::cell::Cell<u64>,
    /// The node the pointer is captured by, if any.
    captured: RefCell<Option<WidgetId>>,
    /// The node with keyboard focus, which receives injected key and character
    /// events as it would from a real backend.
    focused: RefCell<Option<WidgetId>>,
    /// The windows currently being composited, so a painter that captures its
    /// own window (re-entering `render`) is refused instead of recursing.
    rendering: RefCell<HashSet<u64>>,
    text: OnceCell<CosmicShaper>,
    /// The dots-per-inch `Backend::open_window` renders new windows at.
    dpi: u32,
    /// Work to do when the loop "runs": a scripted session for a headless
    /// render, taken and executed once by `Backend::run`.
    run_hook: RefCell<Option<Box<dyn FnOnce()>>>,
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
            captured: RefCell::new(None),
            focused: RefCell::new(None),
            rendering: RefCell::new(HashSet::new()),
            text: OnceCell::new(),
            dpi: DEFAULT_DPI,
            run_hook: RefCell::new(None),
        }
    }

    /// A backend whose windows are rendered at `dpi` (the layout is in design
    /// units, so a higher value gives a proportionally larger image).
    pub fn with_dpi(dpi: u32) -> OffscreenBackend {
        OffscreenBackend {
            dpi,
            ..OffscreenBackend::new()
        }
    }

    /// Sets the work `Backend::run` performs. The offscreen loop has nothing
    /// to wait for, so `run` executes this once, after the app is built and
    /// while its window is still open, then returns.
    pub fn set_run_hook(&self, hook: impl FnOnce() + 'static) {
        *self.run_hook.borrow_mut() = Some(Box::new(hook));
    }

    /// The last icon set on `window` with
    /// [`Backend::set_window_icon`](xui_core::backend::Backend::set_window_icon),
    /// so an app's tests can check that it set one.
    pub fn window_icon(&self, window: WindowId) -> Option<Image> {
        self.windows
            .borrow()
            .get(&window.raw())
            .and_then(|entry| entry.icon.clone())
    }

    /// How many windows are open.
    pub fn window_count(&self) -> usize {
        self.windows.borrow().len()
    }

    /// Delivers a wake to `window`'s sink, so queued messages reach the app's
    /// `update` as they would when a window system wakes the loop.
    pub fn pump(&self, window: WindowId) {
        let sink = self
            .windows
            .borrow()
            .get(&window.raw())
            .and_then(|entry| entry.sink.clone());
        if let Some(sink) = sink {
            sink.deliver(WidgetId::NONE, &Event::Wake);
        }
    }

    fn allocate(cell: &std::cell::Cell<u64>) -> u64 {
        let id = cell.get();
        cell.set(id + 1);
        id
    }

    /// Opens a window rendered at `dpi`, so a test can exercise high-DPI
    /// layouts. [`xui_core::backend::Backend::open_window`] uses
    /// the backend's own DPI.
    pub fn open_window_at(&self, spec: &PlatformSpec, dpi: u32) -> BackendResult<WindowId> {
        let id = WindowId::from_raw(Self::allocate(&self.next_window));
        let width = spec.width.to_px(dpi).value().max(1) as u32;
        let height = spec.height.to_px(dpi).value().max(1) as u32;
        self.windows.borrow_mut().insert(
            id.raw(),
            OffscreenWindow {
                surface: Surface::new(width, height),
                sink: None,
                theme: Theme::light(),
                dpi,
                width: width as i32,
                height: height as i32,
                icon: None,
            },
        );
        Ok(id)
    }

    /// Renders `window`'s visible nodes, in creation order, into an image.
    pub fn render(&self, window: WindowId) -> Option<RgbaImage> {
        // A painter may ask to capture its own window, which would call `render`
        // again; refuse the nested render rather than recursing forever. The
        // guard clears this on every exit, including the early `?` below.
        if !self.rendering.borrow_mut().insert(window.raw()) {
            return None;
        }
        let _guard = RenderGuard {
            rendering: &self.rendering,
            raw: window.raw(),
        };
        // Take the surface out of the window state so a painter that re-enters
        // the backend (for example `Ui::dpi`, which reads this same map) does not
        // find it borrowed. The window stays in the map, with a placeholder
        // surface, so painters still read its live DPI and theme.
        let (dpi, theme, surface) = {
            let mut windows = self.windows.borrow_mut();
            let entry = windows.get_mut(&window.raw())?;
            let surface = std::mem::replace(&mut entry.surface, Surface::new(1, 1));
            (entry.dpi, entry.theme, surface)
        };
        // The guard puts the real surface back on every exit, including a panic
        // in a painter, so a later frame never keeps the 1x1 placeholder.
        let mut restore = SurfaceRestore {
            backend: self,
            raw: window.raw(),
            surface: Some(surface),
        };
        let surface = restore.surface.as_mut().expect("a taken surface");
        surface.fill(theme.background);
        let nodes = self.nodes.borrow();
        let paints: Vec<Draw> = nodes
            .iter()
            .filter(|(id, node)| node.window == window && effectively_visible(&nodes, *id))
            .filter_map(|(id, node)| {
                let painter = node.painter.clone();
                painter.as_ref()?;
                let bounds = absolute_bounds(&nodes, *id)?;
                let clip = ancestor_clip(&nodes, *id);
                if let Some(clip) = clip
                    && intersect(bounds, clip).is_empty()
                {
                    return None;
                }
                Some(Draw {
                    bounds,
                    clip,
                    painter,
                })
            })
            .collect();
        drop(nodes);
        for draw in paints {
            if let Some(painter) = draw.painter {
                surface.with_canvas_at(draw.bounds, dpi, |canvas| {
                    if let Some(clip) = draw.clip {
                        canvas.push_clip(clip);
                    }
                    painter(canvas);
                });
            }
        }
        let image = surface.to_image();
        drop(restore);
        Some(image)
    }

    /// Delivers a positionless event to the focused node of `window`.
    fn inject_to_focus(&self, window: WindowId, event: &Event) -> bool {
        let Some(target) = *self.focused.borrow() else {
            return false;
        };
        let in_window = self
            .nodes
            .borrow()
            .iter()
            .any(|(id, node)| *id == target && node.window == window);
        if !in_window {
            return false;
        }
        let sink = self
            .windows
            .borrow()
            .get(&window.raw())
            .and_then(|entry| entry.sink.clone());
        sink.is_some_and(|sink| sink.deliver(target, event))
    }

    /// Records `id` as the node with keyboard focus.
    pub(super) fn set_focus(&self, id: WidgetId) {
        *self.focused.borrow_mut() = Some(id);
    }

    /// Forgets the focus when its node is gone.
    pub(super) fn forget_focus_if_gone(&self) {
        let mut focused = self.focused.borrow_mut();
        if let Some(id) = *focused
            && !self.nodes.borrow().iter().any(|(node, _)| *node == id)
        {
            *focused = None;
        }
    }

    /// Delivers `event` as a window system would, returning whether it was
    /// consumed. A pointer event goes to the topmost node under its position,
    /// and a captured node receives every pointer move and release, even
    /// outside its bounds. An event without a position (a key or a character)
    /// goes to the focused node in `window`, if any.
    pub fn inject(&self, window: WindowId, event: Event) -> bool {
        let Some((x, y)) = event.position() else {
            return self.inject_to_focus(window, &event);
        };
        let captured = *self.captured.borrow();
        let target = match captured {
            Some(id) => {
                let nodes = self.nodes.borrow();
                let abs = absolute_bounds(&nodes, id);
                abs.map(|abs| (id, x - abs.left, y - abs.top))
            }
            None => {
                let nodes = self.nodes.borrow();
                nodes.iter().rev().find_map(|(id, node)| {
                    if node.window != window {
                        return None;
                    }
                    let abs = hit_bounds(&nodes, *id, x, y)?;
                    Some((*id, x - abs.left, y - abs.top))
                })
            }
        };
        let Some((target, lx, ly)) = target else {
            return false;
        };
        let sink = self
            .windows
            .borrow()
            .get(&window.raw())
            .and_then(|entry| entry.sink.clone());
        let event = translate(event, lx, ly);
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
