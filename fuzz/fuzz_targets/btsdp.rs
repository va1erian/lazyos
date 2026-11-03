#![no_main]
//! Arbitrary bytes as SDP attribute lists, as one response PDU, and as a script
//! of response PDUs for one search (`btsdp::fuzz::run`): nothing may panic and
//! the client stays within its bounds.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| btsdp::fuzz::run(data));
