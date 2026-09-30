#![no_main]
//! `Ring::attach` against an arbitrary, hostile header.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| framering::fuzz::run_header(data));
