#![no_main]
//! A script of push/pop/arm/scribble operations against the frame ring and its
//! reference model (`framering::fuzz::run`).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| framering::fuzz::run(data));
