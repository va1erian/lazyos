#![no_main]
//! `usbhid::fuzz::run_desc`: any bytes as USB device and configuration descriptors.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| usbhid::fuzz::run_desc(data));
