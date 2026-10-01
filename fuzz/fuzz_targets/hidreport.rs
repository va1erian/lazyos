#![no_main]
//! `usbhid::fuzz::run_report`: a script of boot keyboard and mouse reports against a reference model.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| usbhid::fuzz::run_report(data));
