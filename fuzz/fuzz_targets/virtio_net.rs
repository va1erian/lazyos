#![no_main]
//! The virtio-net parsers: device config, receive completions, the packet
//! header and the clamped settings (`virtio_net::fuzz::run`).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| virtio_net::fuzz::run(data));
