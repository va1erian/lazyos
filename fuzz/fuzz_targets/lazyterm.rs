#![no_main]
//! Arbitrary output bytes, sizes and resizes against the Terminal's engine and
//! its consistency checks (`lazyterm::fuzz::run`).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| lazyterm::fuzz::run(data));
