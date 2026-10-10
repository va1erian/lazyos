#![no_main]
//! Arbitrary bytes as a management frame (parsed, summarised as a BSS, fed to
//! a scan table, rebuilt), as an RSN element (parse/build round trip) and as a
//! scan-table script (`ieee80211::fuzz::run`): nothing may panic, and every
//! slice a parse returns lies inside the input.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| ieee80211::fuzz::run(data));
