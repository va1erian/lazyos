#![no_main]
//! `usbmsc::fuzz::run_reply`: any bytes as a CSW, INQUIRY, sense, capacity and mode-sense reply.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| usbmsc::fuzz::run_reply(data));
