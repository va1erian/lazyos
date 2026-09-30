#![no_main]
//! Arbitrary bytes as a LazyOS application package (`lazypkg::fuzz::run`).
//!
//! On a successful open the entry point also reads every entry and calls
//! `digest`/`install_dir`, so the coverage feedback reaches the inflate and
//! CRC paths too, not just the central-directory parser.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| lazypkg::fuzz::run(data));
