#![no_main]
//! Arbitrary bytes as a Bluetooth bond file, verbatim, re-sealed with a correct
//! CRC and wrapped as a version-1 body (`btbond::fuzz::run`): nothing may panic,
//! and whatever decodes encodes back to the same bytes.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| btbond::fuzz::run(data));
