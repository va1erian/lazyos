#![no_main]
//! Arbitrary bytes as a controller firmware file (Intel `.sfi` in every layout,
//! `.ddc`, Realtek epatch v1/v2), as a controller's version answer, and as a
//! script of events, files, ticks and detaches for a loader (`btfw::fuzz::run`):
//! nothing may panic, every command sent is a whole HCI packet, and a finished
//! loader stays finished.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| btfw::fuzz::run(data));
