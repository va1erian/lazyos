#![no_main]
//! Arbitrary bytes as an EAPOL-Key PDU (parse/encode round trip), as key data,
//! and as a supplicant script of frames (`eapol::fuzz::run`): nothing may
//! panic, a refused frame leaves the state as it was, and the replay counter
//! never goes down.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| eapol::fuzz::run(data));
