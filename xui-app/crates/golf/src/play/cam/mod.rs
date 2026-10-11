//! The shot camera director (docs/golf-gameplay-plan.md, section 4). Each
//! shot is a function of the game state that returns a [`Camera`], so a
//! cut is just a different function and the whole thing runs without a
//! window.

pub mod address;
pub mod director;
pub mod obstacles;

pub use address::{address, Address, Style, MIN_CLEARANCE, PUTT_CLEARANCE};
pub use director::Director;
pub use obstacles::Obstacles;
