//! Running an app in the desktop's theme (issue #542).
//!
//! xui opens every window in its light theme; an app that forgets to apply
//! the desktop's mode and accent paints light on a dark desktop. Every desktop
//! app starts through [`run_themed`] instead of `run_app`, so the theme is set
//! before `make` builds a widget and custom painters can read it from
//! [`Ui::theme_handle`](xui_core::app::Ui::theme_handle).

use std::rc::Rc;

use xui_core::app::{run_app, App, Ui};
use xui_core::backend::{Backend, PlatformSpec, Result};

use crate::backend::LazyOSBackend;

/// `run_app` with the desktop's theme applied to the window first. Without a
/// compositor (owner mode) the window keeps xui's default theme.
pub fn run_themed<A, F>(backend: &Rc<LazyOSBackend>, spec: PlatformSpec, make: F) -> Result<()>
where
    A: App,
    F: FnOnce(&mut Ui<A::Msg>) -> A,
{
    let theme = backend.desktop_theme();
    run_app(Rc::clone(backend) as Rc<dyn Backend>, spec, move |ui| {
        if let Some(theme) = theme {
            ui.set_theme(theme);
        }
        make(ui)
    })
}
