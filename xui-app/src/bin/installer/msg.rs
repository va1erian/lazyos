//! The installer window's application messages.
//!
//! Widget event mappers raise these; [`crate::Installer::update`] runs them.
//! Service calls happen there, never inside a widget's own borrow, so an event
//! handler and a `pkgd` call can never overlap.

use std::path::PathBuf;

use xui_core::backend::TimerId;

/// One application message.
#[derive(Clone, Debug)]
pub enum Msg {
    /// Re-query `pkgd.List`.
    Reload,
    /// "Install a package…": open the wizard on its Choose step.
    StartInstall,
    /// The Choose step's path field changed.
    PathChanged(String),
    /// Open the file picker from the Choose step.
    Browse,
    /// The file picker returned a package path.
    Picked(PathBuf),
    /// The file picker was cancelled.
    PickCancelled,
    /// The Choose step's Next: inspect the path in the field.
    Inspect,
    /// The Review step's Next.
    Next,
    /// One wizard step back.
    Back,
    /// Install the package on the Permissions step.
    Install,
    /// Cancel / Close: leave the wizard or the confirmation.
    Cancel,
    /// The success screen's Finish.
    Done,
    /// The Remove button on the row for this system name was clicked. The id,
    /// not a row index, so a stale click can never remove the wrong app.
    AskRemove(String),
    /// Confirm the removal on the confirmation screen.
    ConfirmRemove,
    /// Esc: cancel the current screen (or quit from the list).
    Escape,
    /// The `q` key: quit, but only from the list.
    KeyQ,
    /// The window close button.
    Quit,
    /// A timer fired; the install timer uses this to run outside the click.
    Tick(TimerId),
}
