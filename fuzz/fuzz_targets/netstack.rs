#![no_main]
//! The network stack (smoltcp over a frame ring, DHCP, the echo path) against
//! hostile frames, a gateway that lies and a clock that jumps
//! (`netstack::fuzz::run`).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| netstack::fuzz::run(data));
