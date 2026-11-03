#![no_main]
//! Arbitrary bytes as an L2CAP signalling payload and as an ACL fragment script
//! against the channel manager (`btl2cap::fuzz::run`): nothing may panic,
//! reassembly stays bounded, and a link never holds more channels than its limit.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| btl2cap::fuzz::run(data));
