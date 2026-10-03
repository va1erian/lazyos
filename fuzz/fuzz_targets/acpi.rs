#![no_main]
//! `acpi::fuzz::run`: any bytes as a physical-memory image holding ACPI tables.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| acpi::fuzz::run(data));
