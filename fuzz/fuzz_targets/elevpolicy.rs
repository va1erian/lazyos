#![no_main]
//! Arbitrary bytes as an `elevd` request (`elevpolicy::fuzz::run`): an
//! operation name and NUL-separated arguments. Nothing may panic, what the
//! operation table accepts must round-trip, and no password may reach the
//! prompt's text.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| elevpolicy::fuzz::run(data));
