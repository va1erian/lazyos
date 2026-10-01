#![no_main]
//! The driver engine against a fake device and a hostile client: a script of
//! deliveries, pushes, pops, control calls and scribbles, checked against a
//! reference model (`nicdrv::fuzz::run`).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| nicdrv::fuzz::run(data));
