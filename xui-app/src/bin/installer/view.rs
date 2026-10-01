//! The installer's widget tree: one enum of per-screen views, rebuilt from the
//! model whenever the screen changes.
//!
//! Rebuilding rather than showing/hiding is what makes an arbitrary number of
//! installed apps, permissions and problems work: a screen is constructed from
//! the current model, so a list that grows from zero to dozens of rows simply
//! gets a fresh `ScrollView`/`ListView` with the right content. Every screen
//! owns its widgets, so dropping the previous view destroys its nodes.

use xui_core::app::Ui;
use xui_core::Rect;

use xui_app::installer::{Model, Screen};

use crate::consent::ConsentScreen;
use crate::list_screen::ListScreen;
use crate::msg::Msg;
use crate::simple::{ConfirmScreen, DoneScreen, InstallingScreen};

/// The outer margin every screen keeps.
pub const MARGIN: i32 = 12;

/// A rectangle from a left/top corner and a size.
pub fn rect(x: i32, y: i32, width: i32, height: i32) -> Rect {
    Rect::new(x, y, x + width, y + height)
}

/// Renders a widget-construction error as the friendly string the app logs.
pub fn fail<E: std::fmt::Display>(error: E) -> String {
    error.to_string()
}

/// The widgets of the currently shown screen.
///
/// The payloads are only ever held, never read: owning them is what keeps
/// their nodes alive for the life of the screen.
#[allow(dead_code)]
pub enum View {
    /// The installed list.
    List(ListScreen),
    /// The consent screen.
    Consent(ConsentScreen),
    /// The install progress screen.
    Installing(InstallingScreen),
    /// The install success screen.
    Done(DoneScreen),
    /// The remove confirmation.
    Confirm(ConfirmScreen),
}

/// Builds the view for `model`'s current screen at the window's client size.
pub fn build(ui: &Ui<Msg>, model: &Model) -> Result<View, String> {
    let bounds = ui.client_rect();
    // A window may briefly report an empty client rect; a floor keeps the
    // widgets' rectangles non-negative until the first resize rebuilds them.
    let width = bounds.width().max(360);
    let height = bounds.height().max(260);
    match model.screen {
        Screen::List => Ok(View::List(ListScreen::build(ui, width, height, model)?)),
        Screen::Consent => Ok(View::Consent(ConsentScreen::build(
            ui, width, height, model,
        )?)),
        Screen::Installing => Ok(View::Installing(InstallingScreen::build(
            ui, width, height, model,
        )?)),
        Screen::Done => Ok(View::Done(DoneScreen::build(ui, width, height, model)?)),
        Screen::ConfirmRemove => Ok(View::Confirm(ConfirmScreen::build(
            ui, width, height, model,
        )?)),
    }
}
