//! The engine of `audiod`, the system mixer (docs/audio-plan.md stage A1).
//!
//! Every application stream is a client-owned ring of interleaved `S16Le`
//! frames at the client's own rate. Each output period the engine pulls what
//! each running stream has committed, converts it to the card's rate
//! ([`resample`]), scales it by the stream's gain ([`gain`]), sums the streams
//! in wide accumulators, scales the sum by the master gain and writes one
//! saturated stereo `S16Le` period for the card.
//!
//! It is pure `no_std` + `alloc` logic with no syscalls, so the whole
//! `os.lazy.audio.v1` stream contract (commit limits, ownership, drain,
//! positions) is tested on the host; `audiod` only adds Messenger, the card
//! and the raw copy out of mapped client memory ([`Ring`]). Everything a
//! client says is checked here, and a client rewriting its ring concurrently
//! can only produce noise in its own stream.
//!
//! User programs are soft-float, so everything is fixed point.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod events;
pub mod gain;
pub mod grant;
mod mixer;
pub mod resample;
pub mod service;
mod stream;
pub mod volume;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_events;
#[cfg(test)]
mod tests_fuzz;
#[cfg(test)]
mod tests_mix;
#[cfg(test)]
mod tests_service;
#[cfg(test)]
mod tests_volume;

pub use mixer::{Config, MixError, Mixer, Status};
pub use stream::State;

/// Channels of the mix: the engine always produces interleaved stereo.
pub const MIX_CHANNELS: usize = 2;

/// A client's sample ring as the mixer sees it.
///
/// `audiod` implements it over a shared buffer mapped into its address space
/// (a raw copy, never a reference to memory the client can rewrite); the host
/// tests over a plain vector.
pub trait Ring {
    /// Bytes in the ring.
    fn len(&self) -> usize;

    /// Whether the ring holds no bytes at all.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Copy `dst.len()` bytes starting at `offset`. The engine only asks for
    /// ranges inside `0..len()`.
    fn read(&self, offset: usize, dst: &mut [u8]);
}
