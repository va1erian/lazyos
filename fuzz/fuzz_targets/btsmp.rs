#![no_main]
//! Arbitrary bytes as an SMP PDU (decode/encode round trip) and as a script of
//! inputs for the LE Secure Connections central (`btsmp::fuzz::run`): nothing
//! may panic, an ended pairing stays silent, and everything the central sends
//! parses.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| btsmp::fuzz::run(data));
