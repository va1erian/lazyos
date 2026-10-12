#![no_main]
//! `gpt::fuzz::run`: whole disks and damaged GUID Partition Tables
//! (docs/grub-install-plan.md K2).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| gpt::fuzz::run(data));
