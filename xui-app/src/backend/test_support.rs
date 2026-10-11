//! Host-test scaffolding for the backend: a client-mode backend with no
//! display behind it, and small windows whose events go to a sink.

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
