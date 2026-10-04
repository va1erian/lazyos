#![no_main]
//! Arbitrary bytes as a PWG Raster stream (`raster::fuzz::run`): decoding is
//! bounded, and a stream that decodes re-encodes to the same pixels.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| raster::fuzz::run(data));
