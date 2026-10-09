#![no_main]
//! Arbitrary bytes as a `dbgd` request line, a `lazyos.cfg` and a boot-log
//! line (`dbgwire::fuzz::run`): nothing may panic, a parsed JSON value must
//! write back to the same JSON.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| dbgwire::fuzz::run(data));
