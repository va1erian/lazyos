#![forbid(unsafe_code)]

//! A resizable headless backend for the xui-app test suites.
//!
//! `xui-core`'s own headless backend can resize a window and deliver
//! [`Event::Resize`], but it is crate-private, and `xui-canvas`'s offscreen
//! backend cannot change a window's client rect. This wrapper forwards every
//! backend operation to the offscreen backend (nodes, painting, text) and adds
//! [`TestBackend::resize_window`], so a test can grow and shrink a window and
//! watch the app re-flow, as the compositor does through `Configure`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use xui_canvas::OffscreenBackend;
use xui_core::backend::{
    Backend, Cursor, Event, FileDialogOutcome, FileDialogRequest, FontSpec, ImplKind, NodeKind,
    NodeSpec, Painter, ParentRef, PlatformSpec, Result, TextLayout, TextMetrics, TextShaper,
    TextStyle, TimerId, Waker, WidgetId, WindowId,
};
use xui_core::geometry::Rect;
use xui_core::image::Image;
use xui_core::router::WidgetHost;
use xui_core::theme::Theme;
use xui_core::units::Dip;

/// The offscreen backend plus a mutable client size and the event sink per
/// window, so a test can deliver a window-level [`Event::Resize`].
pub struct TestBackend {
    inner: Rc<OffscreenBackend>,
    size: RefCell<HashMap<u64, (i32, i32)>>,
    sinks: RefCell<HashMap<u64, Rc<dyn WidgetHost>>>,
}

impl TestBackend {
    /// A backend with no windows.
    pub fn new() -> TestBackend {
        TestBackend {
            inner: Rc::new(OffscreenBackend::new()),
            size: RefCell::new(HashMap::new()),
            sinks: RefCell::new(HashMap::new()),
        }
    }

    /// Sets the work `Backend::run` performs, as
    /// [`OffscreenBackend::set_run_hook`] does: a test resizes the window and
    /// asserts in the hook, after the app is built and before the loop returns.
    pub fn set_run_hook(&self, hook: impl FnOnce() + 'static) {
        self.inner.set_run_hook(hook);
    }

    /// Delivers `event` as a window system would and drains the messages it
    /// raised, as [`OffscreenBackend::inject`] does.
    pub fn inject(&self, window: WindowId, event: Event) -> bool {
        let consumed = self.inner.inject(window, event);
        self.inner.pump(window);
        consumed
    }

    /// Resizes `window`'s client area and delivers the window-level
    /// [`Event::Resize`], then drains the messages it raised. This is what the
    /// compositor's `Configure` does in the real backend.
    pub fn resize_window(&self, window: WindowId, width: i32, height: i32) {
        self.size.borrow_mut().insert(window.raw(), (width, height));
        if let Some(sink) = self.sinks.borrow().get(&window.raw()).cloned() {
            sink.deliver(WidgetId::NONE, &Event::Resize { width, height });
        }
        self.inner.pump(window);
    }
}

impl Default for TestBackend {
    fn default() -> TestBackend {
        TestBackend::new()
    }
}

/// Forwards a backend method to the wrapped offscreen backend. `set_event_sink`
/// and `client_rect` are hand-written below because they carry window state.
macro_rules! forward {
    ($($name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)?;)*) => {
        $(
            fn $name(&self $(, $arg: $ty)*) $(-> $ret)? {
                self.inner.$name($($arg),*)
            }
        )*
    };
}

impl Backend for TestBackend {
    fn set_event_sink(&self, window: WindowId, sink: Rc<dyn WidgetHost>) {
        self.sinks.borrow_mut().insert(window.raw(), sink.clone());
        self.inner.set_event_sink(window, sink);
    }

    fn client_rect(&self, window: WindowId) -> Rect {
        match self.size.borrow().get(&window.raw()) {
            Some(&(width, height)) => Rect::new(0, 0, width, height),
            None => self.inner.client_rect(window),
        }
    }

    forward! {
        run() -> i32;
        quit(code: i32);
        wake(window: WindowId);
        waker(window: WindowId) -> Waker;
        open_window(spec: &PlatformSpec) -> Result<WindowId>;
        close_window(window: WindowId);
        set_window_title(window: WindowId, title: &str);
        set_window_icon(window: WindowId, icon: &Image);
        set_window_enabled(window: WindowId, enabled: bool);
        capture(window: WindowId) -> Result<Image>;
        run_modal(window: WindowId) -> Result<()>;
        file_dialog(window: WindowId, request: &FileDialogRequest) -> FileDialogOutcome;
        minimize(window: WindowId);
        toggle_maximize(window: WindowId);
        is_maximized(window: WindowId) -> bool;
        caption_inset(window: WindowId) -> Dip;
        create(parent: ParentRef, spec: &NodeSpec) -> Result<WidgetId>;
        destroy(id: WidgetId);
        apply_moves(window: WindowId, moves: &[(WidgetId, Rect)]);
        set_visible(id: WidgetId, visible: bool);
        set_enabled(id: WidgetId, enabled: bool);
        raise(id: WidgetId);
        set_drag_region(id: WidgetId, drag: bool);
        set_cursor(id: WidgetId, cursor: Cursor);
        set_clip(id: WidgetId, rect: Option<Rect>);
        set_capture(id: WidgetId);
        release_capture();
        focus(id: WidgetId);
        set_text(id: WidgetId, text: &str);
        set_cue(id: WidgetId, cue: &str);
        text(id: WidgetId) -> String;
        clipboard_text() -> Option<String>;
        set_clipboard_text(text: &str);
        bounds(id: WidgetId) -> Rect;
        invalidate(id: WidgetId);
        invalidate_rect(id: WidgetId, rect: Rect);
        set_painter(id: WidgetId, painter: Painter);
        measure_text(text: &str, style: &TextStyle, dpi: u32) -> TextMetrics;
        text_shaper() -> Box<dyn TextShaper>;
        layout_text(text: &str, spec: &FontSpec, max_width: f32, dpi: u32) -> Box<dyn TextLayout>;
        dpi(window: WindowId) -> u32;
        set_theme(window: WindowId, theme: &Theme);
        set_timer(window: WindowId, millis: u32) -> TimerId;
        kill_timer(window: WindowId, id: TimerId);
        supports(kind: NodeKind) -> ImplKind;
    }
}
