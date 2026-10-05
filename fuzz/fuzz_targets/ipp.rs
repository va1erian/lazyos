#![no_main]
//! Arbitrary bytes as an IPP message (`ipp::fuzz::run`): a printer's reply is
//! untrusted network input. A message that decodes must round-trip.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| ipp::fuzz::run(data));
