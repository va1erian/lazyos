#![no_main]
//! An SNTP reply with the exchange's local times, or a schedule script
//! (`sntp::fuzz::run`): nothing may panic, a reply is accepted only for the
//! nonce it echoes, an accepted sample stays inside the delay and distance
//! bounds, and a clock step never lands before the floor, past the kernel's
//! range or beyond the step limit after the first sync.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| sntp::fuzz::run(data));
