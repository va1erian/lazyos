//! The feed model: rows, prefix sums and scroll anchoring (R3). No I/O, no
//! xui type; the UI instantiates it with its own row payload.

pub mod model;
#[cfg(test)]
mod tests;

pub use model::{Feed, FeedRow, RowId, RowKind, LOAD_MORE_ID};
