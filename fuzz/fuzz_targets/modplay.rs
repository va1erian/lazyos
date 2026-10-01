#![no_main]
//! Arbitrary bytes as a ProTracker module (`modplay::fuzz::run`).
//!
//! A parse that succeeds is also rendered for a few seconds with every effect
//! the file selects, so coverage reaches the mixer and the sequencer.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| modplay::fuzz::run(data));
