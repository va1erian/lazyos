#![no_main]
//! `usbmsc::fuzz::run_desc`: any bytes as a configuration chain searched for a Bulk-Only interface.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| usbmsc::fuzz::run_desc(data));
