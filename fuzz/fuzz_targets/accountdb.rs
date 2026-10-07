#![no_main]
//! Arbitrary bytes as the account database (`accountdb::fuzz::run`): a
//! corrupted or hostile `/accounts/db` must fail closed, what parses must
//! satisfy every rule `accountsd` and `keyd` rely on and round-trip, and the
//! account operations run on it must keep those rules and the last admin.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| accountdb::fuzz::run(data));
