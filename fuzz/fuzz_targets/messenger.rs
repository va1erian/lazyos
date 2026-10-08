#![no_main]
//! The Messenger parcel decoder (version 2) against arbitrary bytes
//! (`libmessenger::fuzz::run`).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| libmessenger::fuzz::run(data));
