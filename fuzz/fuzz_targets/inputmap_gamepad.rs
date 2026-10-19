#![no_main]
//! A script of bus records, `SOURCE` registrations (some lost), table reads,
//! loss markers and hostile records against the `inputd` gamepad path and a
//! model of the kernel's source table (`inputmap::fuzz_gamepad::run`).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| inputmap::fuzz_gamepad::run(data));
