#![no_main]
//! Arbitrary bytes as a package manifest or a list of permission lines
//! (`pkgstore::fuzz::run`): validation, `rules::compile` and
//! `explain::permissions` must never panic, stay within the rule budget, refuse
//! `..`/`**` patterns and explain every permission.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| pkgstore::fuzz::run(data));
