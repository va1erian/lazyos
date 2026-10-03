//! A small ProTracker (`.mod`) player: bytes in, interleaved stereo `i16` out.
//!
//! The crate does no I/O and no floating point (user programs are built
//! soft-float), so the same code runs in the `modplay` guest program and in
//! host tests. The file is untrusted input: [`Module::parse`] validates every
//! count and length and the player never indexes by a file-supplied value
//! unchecked.
//!
//! ```ignore
//! let module = Module::parse(&bytes)?;
//! let mut player = Player::new(&module, 48_000, Options::default());
//! let mut buf = [0i16; 2048];
//! while player.render(&mut buf) > 0 { /* hand buf to the audio stream */ }
//! ```
//!
//! A long-lived UI holds the module through an owning pointer instead
//! (`Player::new(Rc::new(module), ..)`) and drives the player live: seek to an
//! order, mute channels, change the stereo separation, read levels.

#![no_std]

extern crate alloc;
#[cfg(any(test, feature = "fuzz"))]
extern crate std;

mod channel;
mod mixer;
mod module;
mod parse;
mod player;
mod tables;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_controls;

#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
#[cfg(any(test, feature = "fuzz"))]
pub mod synth;

pub use module::{ModError, Module, Note, Sample, CHANNELS, ROWS_PER_PATTERN};
pub use player::{Options, Player, Position};
