#![no_main]
//! Arbitrary bytes as an `accountsd` or `keyd` request or reply
//! (`accountwire::run`): the parcel envelope and every generated body decoder
//! of both interfaces. Nothing may panic; a decoded value must round-trip.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| accountwire::run(data));
