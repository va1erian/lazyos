#![no_main]
//! `gen9gfx::fuzz::run`: EDID, ELD, VBT and HDMI N/CTS input
//! (docs/kabylake-gpu-plan.md, stage G0 prep). The first byte picks the
//! parser; any other value runs all three.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| gen9gfx::fuzz::run(data));
