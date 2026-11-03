#![no_main]
//! Arbitrary bytes as an HCI event, command and ACL packet (parse/encode round
//! trip) and as a script of controller events (`bthci::fuzz::run`): nothing may
//! panic, and the command queue and ACL buffer pool keep their bounds.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| bthci::fuzz::run(data));
