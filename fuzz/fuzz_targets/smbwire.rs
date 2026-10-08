#![no_main]
//! An SMB server's bytes (`smbwire::fuzz::run`): every decoder, the framing,
//! and a whole client session over a scripted stream. A server is untrusted
//! network input: nothing may panic, hang or allocate without bound.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| smbwire::fuzz::run(data));
