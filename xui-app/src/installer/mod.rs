//! Pure view-model for the `installer` app (phase 5 of the package system).
//!
//! Nothing here touches widgets or `pkgd`: the state machine ([`Model`]), the
//! risk grouping and the text helpers are host-testable, and
//! `src/bin/installer.rs` renders them. Every field of a package is untrusted
//! (it comes from an archive through `pkgd`), so the view-model only ever hands
//! the screen strings produced by [`view::elide`], which strips control
//! characters and bounds the length — a hostile package cannot forge serial
//! markers, overflow a label, or make the app panic on empty/non-ASCII text.

mod model;
mod view;

pub use model::{Installed, MimeHandler, Model, Package, Permission, Request, Screen};
pub use view::{clean, elide, group_by_risk, permission_line, short_digest, Risk, RiskGroup};
