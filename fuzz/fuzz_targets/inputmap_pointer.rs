#![no_main]
//! A script of raw pointer records, loss markers and resizes against the
//! `inputd` pointer engine and its reference model (`inputmap::fuzz::run`).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| inputmap::fuzz::run(data));
