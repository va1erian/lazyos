#![no_main]
//! Arbitrary bytes as a HIDP message, a report descriptor and a script of input
//! reports (`bthid::fuzz::run`): nothing may panic and every key goes up again.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| bthid::fuzz::run(data));
