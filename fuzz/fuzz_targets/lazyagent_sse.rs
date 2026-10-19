#![no_main]
//! Arbitrary bytes, in any chunking, against LazyAgent's SSE parser and the
//! chunk reader behind it (`lazyagent_core::fuzz::run`).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| lazyagent_core::fuzz::run(data));
