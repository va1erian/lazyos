//! The installer's widget tree: one layout per screen, mounted from the model
//! whenever the screen changes.
//!
//! Rebuilding rather than showing/hiding is what makes an arbitrary number of
//! installed apps, permissions and problems work: a screen is laid out from
//! the current model, so a list that grows from zero to dozens of rows simply
//! gets a fresh `ScrollView`/`ListView` with the right content. The mounted
//! layout owns the screen's widgets, so dropping the previous one destroys its
//! nodes; the layout keeps the screen placed as the window resizes.

use xui_core::app::Ui;
use xui_core::arrange::{Layout, Mounted};

use xui_app::installer::{Model, Screen};

use crate::msg::Msg;
use crate::{consent, list_screen, simple, wizard};

/// The outer margin every screen keeps.
pub const MARGIN: i32 = 12;

/// Renders a widget-construction error as the friendly string the app logs.
pub fn fail<E: std::fmt::Display>(error: E) -> String {
    error.to_string()
}

/// The widgets of the currently shown screen, alive while this is.
pub type View = Mounted<Msg>;

/// The layout of `model`'s current screen.
fn layout(model: &Model) -> Layout<Msg> {
    match model.screen {
        Screen::List => list_screen::layout(model),
        Screen::Choose => wizard::choose(model),
        Screen::Review => consent::review(model),
        Screen::Permissions => consent::permissions(model),
        Screen::Installing => simple::installing(model),
        Screen::Done => simple::done(model),
        Screen::ConfirmRemove => simple::confirm(model),
    }
}

/// Mounts the view for `model`'s current screen on the window.
pub fn build(ui: &Ui<Msg>, model: &Model) -> Result<View, String> {
    ui.mount(layout(model)).map_err(fail)
}
