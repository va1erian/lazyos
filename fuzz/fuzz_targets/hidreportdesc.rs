#![no_main]
//! `usbhid::fuzz::run_pointer_desc` and `run_gamepad_desc`: any bytes as a HID
//! report descriptor and a report, through the pointer, gamepad and X-input
//! decoders.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    usbhid::fuzz::run_pointer_desc(data);
    usbhid::fuzz::run_gamepad_desc(data);
});
