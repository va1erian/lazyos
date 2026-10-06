#![no_main]
//! Arbitrary bytes as the account file (`passwd::fuzz::run`): a corrupted or
//! hostile `/system/etc/passwd` must fail closed, and what parses must
//! satisfy every rule `accountsd` relies on and round-trip.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| passwd::fuzz::run(data));
