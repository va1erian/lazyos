#![no_main]
//! `usbhid::fuzz::run_pointer_desc`: any bytes as a HID report descriptor and a report.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| usbhid::fuzz::run_pointer_desc(data));
