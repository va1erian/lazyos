#![no_main]
//! Arbitrary bytes as a `lazyos.cfg`, a BDF, an allow range and a read
//! request (`pciprobe::fuzz::run`): nothing may panic, and a plan always
//! tiles its request inside allowed ranges.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| pciprobe::fuzz::run(data));
