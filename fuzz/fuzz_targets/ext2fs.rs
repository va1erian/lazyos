#![no_main]
//! A script of filesystem operations checked against a model, or a corrupted
//! image mounted and used (`ext2fs::fuzz::run`).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| ext2fs::fuzz::run(data));
