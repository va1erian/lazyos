#![no_main]
//! `usbaudio::fuzz::run_desc`: any bytes as USB Audio Class descriptors, a range reply and feedback.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| usbaudio::fuzz::run_desc(data));
