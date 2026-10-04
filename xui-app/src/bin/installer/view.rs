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

use crate::consent::{PermissionsScreen, ReviewScreen};
use crate::list_screen::ListScreen;
use crate::msg::Msg;
use crate::simple::{ConfirmScreen, DoneScreen, InstallingScreen};
use crate::wizard::ChooseScreen;

/// The outer margin every screen keeps.
pub const MARGIN: i32 = 12;

/// A rectangle from a left/top corner and a size.
pub fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    // Design pixels, at the desktop's UI scale (docs/hidpi-plan.md).
    xui_app::hidpi::rect(x, y, w, h)
}

/// Renders a widget-construction error as the friendly string the app logs.
pub fn fail<E: std::fmt::Display>(error: E) -> String {
    error.to_string()
}

/// The widgets of the currently shown screen.
///
/// The payloads are only ever held, never read: owning them is what keeps
/// their nodes alive for the life of the screen. Exactly one exists at a time,
/// so the variants' size difference costs nothing worth a box.
#[allow(dead_code, clippy::large_enum_variant)]
pub enum View {
    /// The installed list.
    List(ListScreen),
    /// Wizard step 1: choose the package.
    Choose(ChooseScreen),
    /// Wizard step 2: review the package.
    Review(ReviewScreen),
    /// Wizard step 3: consent to its permissions.
    Permissions(PermissionsScreen),
    /// The install progress screen.
    Installing(InstallingScreen),
    /// The install success screen.
    Done(DoneScreen),
    /// The remove confirmation.
    Confirm(ConfirmScreen),
}

/// Builds the view for `model`'s current screen at the window's client size.
pub fn build(ui: &Ui<Msg>, model: &Model) -> Result<View, String> {
    let bounds = xui_app::hidpi::design_rect(ui);
    // A window may briefly report an empty client rect; a floor keeps the
    // widgets' rectangles non-negative until the first resize rebuilds them.
    let width = bounds.width().max(360);
    let height = bounds.height().max(260);
    match model.screen {
        Screen::List => Ok(View::List(ListScreen::build(ui, width, height, model)?)),
        Screen::Choose => Ok(View::Choose(ChooseScreen::build(ui, width, height, model)?)),
        Screen::Review => Ok(View::Review(ReviewScreen::build(ui, width, height, model)?)),
        Screen::Permissions => Ok(View::Permissions(PermissionsScreen::build(
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
