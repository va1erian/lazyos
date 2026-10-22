//! The places the person keeps: bookmarks (docs/lazywebplus-plan.md, section 4). A pure-Rust store
//! with no window and no LazyOS in it, tested and fuzzed on the host.
//!
//! - [`model`]: an entry, the three roots, the caps, what text and addresses may be kept.
//! - [`log`]: the bytes of `bookmarks.log` (length-prefixed, checksummed records).
//! - [`Bookmarks`]: the entries in memory and the log that keeps them.
//!
//! The file is the browser's own, in its data folder (`$HOME/.apps/os.lazy.lazywebplus/`), written
//! only by the window and read back without trusting it.

pub mod log;
pub mod model;
mod store;

#[cfg(test)]
mod fuzz;
#[cfg(test)]
mod tests;

pub use model::{Entry, Error, Id, Kind, BAR, OTHER, UNSORTED};
pub use store::Bookmarks;
