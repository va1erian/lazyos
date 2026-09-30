//! PCM sample helpers shared by the audio driver and its clients.
//!
//! Pure `no_std` logic with host tests. User programs are built soft-float, so
//! everything here is fixed point.

#![no_std]

#[cfg(test)]
extern crate std;

pub mod tone;
