#![forbid(unsafe_code)]

//! The Wi-Fi half of the Network Status tray applet, as pure functions.
//!
//! There is no Wi-Fi outline in the named-icon library (`lazyicons`, a
//! pinned xui revision), so the applet draws its own icon as an alpha mask
//! the shell tints with the bar's ink ([`glyph`]); a mask reads on light and
//! dark bars alike. [`Link`] is what the applet knows of the radio;
//! [`model::show`] turns it into the tooltip, the icon and the item's
//! status, and [`menu::build`] turns the heard networks into the tray menu
//! and remembers what each row does ([`menu::Action`]).
//!
//! Names come from access points anyone can run: they are shown as text
//! only, clipped here again, and never put in a command.

pub mod glyph;
pub mod menu;
pub mod model;

pub use glyph::{Glyph, Mask};
pub use menu::{Action, Menu, NetworkRow};
pub use model::{show, Link, Shown, State};

/// The argument that makes the Network app open on its Wi-Fi page. `init`'s
/// `Launch` takes one absolute path as its argument, so the page is named
/// like a path: the Network app reads `/wifi` as "start on the Wi-Fi page".
pub const NETWORK_WIFI_ARG: &str = "/wifi";
