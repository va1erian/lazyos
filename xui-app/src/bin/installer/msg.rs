//! The installer window's application messages.
//!
//! Widget event mappers raise these; [`crate::Installer::update`] runs them.
//! Service calls happen there, never inside a widget's own borrow, so an event
//! handler and a `pkgd` call can never overlap.

use xui_core::backend::TimerId;

/// One application message.
#[derive(Clone, Debug)]
pub enum Msg {
    /// Re-query `pkgd.List`.
    Reload,
    /// The path field's text changed.
    PathChanged(String),
    /// Inspect the path in the field.
    Inspect,
    /// Install the package on the consent screen.
    Install,
    /// Cancel / Close / Back.
    Cancel,
    /// The success screen's Done.
    Done,
    /// The Remove button on the row for this system name was clicked. The id,
    /// not a row index, so a stale click can never remove the wrong app.
    AskRemove(String),
    /// Confirm the removal on the confirmation screen.
    ConfirmRemove,
    /// Esc: cancel the current screen (or quit from the list).
    Escape,
    /// The `q` key: quit, but only from the list with an empty path field.
    KeyQ,
    /// The window close button.
    Quit,
    /// The compositor resized the window; re-lay the screen out.
    Resize,
    /// A timer fired; the install timer uses this to run outside the click.
    Tick(TimerId),
}
