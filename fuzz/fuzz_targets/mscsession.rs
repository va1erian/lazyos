#![no_main]
//! `usbmsc::fuzz::run_session`: a whole hostile mass-storage device answering from the bytes.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| usbmsc::fuzz::run_session(data));
