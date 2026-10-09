#![no_main]
//! `ahci::fuzz::run`: IDENTIFY pages, PRDT plans and a hostile HBA's every
//! answer (docs/ahci-plan.md A1).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| ahci::fuzz::run(data));
