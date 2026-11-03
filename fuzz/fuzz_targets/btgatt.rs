#![no_main]
//! Arbitrary bytes as an ATT PDU (answer to every request kind, and
//! unsolicited) and as a script of server answers for the HID over GATT driver
//! (`btgatt::fuzz::run`): nothing may panic, one request is in flight at most,
//! and what discovery reports stays within its bounds.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| btgatt::fuzz::run(data));
