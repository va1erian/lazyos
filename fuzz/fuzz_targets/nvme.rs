#![no_main]
//! `nvme::fuzz::run`: Identify pages, completion entries, PRP plans and a
//! hostile controller's every answer (docs/nvme-install-plan.md N1).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| nvme::fuzz::run(data));
