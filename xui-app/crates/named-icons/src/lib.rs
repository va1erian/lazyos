//! `lazyicons`: the OS-wide names of LazyOS's outline icons
//! (docs/tray-plan.md §6.3, catalog in `docs/icons.md`).
//!
//! An app, the shell, a LazyRAD form or a script names an icon with a short
//! kebab-case word (`"save"`, `"volume-2"`) instead of a toolkit enum, so the
//! name can cross a process boundary (the tray protocol, a package manifest,
//! a form file) and still mean the same picture years later. The mapping
//! lives here, on the OS side, rather than in xui: the names are a stable API
//! that packages depend on, while the toolkit's [`Lucide`] enum may be
//! reshaped.
//!
//! # The contract
//!
//! * **Append-only.** Once a name has shipped it is never removed and never
//!   redrawn as a different picture. When an outline is replaced upstream,
//!   its old name stays as an alias of the replacement.
//! * **Exact.** Lookups are exact and case-sensitive: `"Save"` and `" save"`
//!   are unknown. An unknown name is `None`, never a silent substitute; the
//!   caller picks its own fallback.
//! * **Growth by request.** A new icon is added by vendoring its outline in
//!   xui (`assets/lucide/`), bumping the xui revision, then appending its
//!   name to the table (`names.rs`, which fails to compile until you do).
//!
//! # Use from Rust
//!
//! ```
//! use xui_core::Lucide;
//! assert_eq!(lazyicons::from_name("save"), Some(Lucide::Save));
//! assert_eq!(lazyicons::name(Lucide::Volume2), "volume-2");
//! assert_eq!(lazyicons::from_name("Save"), None);
//! ```
//!
//! [`draw`] paints a name onto any xui [`Canvas`].

#![forbid(unsafe_code)]

mod catalog;
mod names;

pub use catalog::catalog_markdown;
pub use names::name;

use xui_core::backend::Canvas;
use xui_core::{draw_icon, Color, Lucide, Rect};

/// The longest name [`from_name`] considers. Every name is far shorter (the
/// longest today is 24 bytes); the cap lets a lookup of untrusted input (a
/// name off the wire) refuse a huge string before comparing anything.
pub const MAX_NAME_LEN: usize = 64;

/// The outline called `name`, or `None` when no icon has that exact name.
///
/// Accepts the canonical names [`all`] lists and the aliases kept for
/// outlines replaced upstream. Safe on untrusted input: anything empty or
/// longer than [`MAX_NAME_LEN`] is refused before the table is searched.
pub fn from_name(name: &str) -> Option<Lucide> {
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return None;
    }
    // A linear scan: about a hundred short comparisons, cheaper than building
    // and keeping a map, and the table stays a single exhaustive `match`.
    all()
        .chain(names::ALIASES.iter().copied())
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, icon)| icon)
}

/// Every icon with its canonical name, one entry per [`Lucide`] variant.
///
/// The order is xui's (`Lucide::ALL`, by upstream file name) and is not part
/// of the contract; aliases are not listed.
pub fn all() -> impl Iterator<Item = (&'static str, Lucide)> {
    Lucide::ALL.iter().map(|&icon| (name(icon), icon))
}

/// Draws the outline called `name` into `rect` in `ink`, at `dpi` (96 is 1x),
/// with the stroke width Lucide expects for that size, through xui's own
/// [`draw_icon`] so every app renders a name identically.
///
/// Returns `false`, drawing nothing, when `name` is unknown: the caller
/// decides what to show instead.
pub fn draw(canvas: &mut dyn Canvas, name: &str, rect: Rect, ink: Color, dpi: u32) -> bool {
    match from_name(name) {
        Some(icon) => {
            draw_icon(canvas, icon, rect, ink, dpi);
            true
        }
        None => false,
    }
}
