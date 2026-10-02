//! The pure logic of `pkgd`, the LazyOS package manager (`docs/packages.md`,
//! phase 3).
//!
//! `pkgd` is a root service that extracts packages, loads kernel policy and
//! writes an audit log, so every decision that does not need a syscall lives
//! here, where `cargo test` can pin it:
//!
//! * [`access`]: who may install or remove, and which package files `pkgd`
//!   (a root service) will read on a caller's behalf;
//! * [`rules`]: what a manifest's permissions compile to in kernel
//!   [`LabelRule`](messenger_generated::os_lazy_messenger_policy_v1::LabelRule)s,
//!   so what the installer *shows* and what the kernel *enforces* come from one
//!   function;
//! * [`explain`]: the plain-language sentence and risk for every permission,
//!   keyed by MIDL interface name (a test proves it covers every `idl/*.midl`
//!   interface);
//! * [`audit`]: the hash-chained `/logs/pkg.log` format and its verifier;
//! * [`layout`]: install directory paths and the extraction plan, with the
//!   path-safety checks a root writer must make even for a validated package;
//! * [`docs`]: where an app's documentation goes (`/docs/apps/<system_name>`)
//!   and how an interrupted replacement is repaired;
//! * [`tree`]: extraction, documentation and removal over a [`tree::TreeFs`],
//!   the same code under `pkgd`'s syscalls, the host tests and the kernel
//!   suite;
//! * [`hash`]: the FNV-1a hashes the kernel keys policy by.
//!
//! The crate is `no_std` + `alloc` and touches no syscall: [`tree`] reaches
//! the filesystem only through the trait its caller implements.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod access;
pub mod audit;
pub mod docs;
pub mod explain;
pub mod hash;
pub mod layout;
pub mod rules;
pub mod tree;
