//! The desktop notifications' model (docs/notify-plan.md, issue #747): what
//! is shown as a toast, in which order, for how long, and how it moves. The
//! presenter in `xui-app/src/shell/notify` draws it and serves the
//! `os.lazy.notify` interface; every decision lives here, so it is tested on
//! the host with no clock, no screen and no service.
//!
//! * [`note`]: the validated notification and the checks a request goes
//!   through.
//! * [`policy`]: who may notify and as whom, the rate limit, the user's
//!   do-not-disturb and per-app mute.
//! * [`center`]: the queue: places, waiting, coalescing, replace in place,
//!   timeouts, hover pause, and the tweens read as [`frame::Frame`]s.
//! * [`settings`]: the `sys/ui/motion` and `sys/ui/notify/*` keys.
//! * [`card`]: a toast's face: text wrapping, geometry, hit test.
//! * [`frame`]: what is drawn at one instant, and the icon flourishes.

pub mod card;
pub mod center;
pub mod frame;
pub mod note;
pub mod policy;
pub mod settings;

#[cfg(test)]
mod fuzz;
#[cfg(test)]
mod tests;

pub use center::{Center, Event, EventKind, Refused};
pub use frame::{Frame, Stage};
pub use note::{CloseReason, Level, Note, Request};
