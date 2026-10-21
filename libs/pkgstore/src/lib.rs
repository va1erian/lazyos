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
//!   function; and what each `[processes.<name>]` table (a helper program with
//!   a narrower label, issue #832) compiles to, and which labels an upgrade or
//!   a removal drops;
//! * [`explain`]: the plain-language sentence and risk for every permission,
//!   keyed by MIDL interface name (a test proves it covers every `idl/*.midl`
//!   interface);
//! * [`filerules`]: what a sandboxed process table's `files` compile to (the
//!   rule ids the kernel's file gate looks for), by `libs/filepolicy`;
//! * [`audit`]: the hash-chained `/logs/pkg.log` format and its verifier;
//! * [`layout`]: install directory paths and the extraction plan, with the
//!   path-safety checks a root writer must make even for a validated package;
//! * [`docs`]: where an app's documentation goes (`/docs/apps/<system_name>`)
//!   and how an interrupted replacement is repaired;
//! * [`tree`]: extraction, documentation and removal over a [`tree::TreeFs`],
//!   the same code under `pkgd`'s syscalls, the host tests and the kernel
//!   suite;
//! * [`provision`]: which core packages (`/system/packages`) to install,
//!   upgrade, keep or demote, the provisioning stamp, and the refusals a core
//!   app adds to `Remove` and `Install`;
//! * [`develop`]: development labels (`dev:<system_name>`, issue #529): the
//!   rules `Develop` loads and the in-memory record of what was approved;
//! * [`inspect`]: an archive turned into the `PackageInfo` the consent screen
//!   shows, shared by `pkgd`'s `Inspect` and `Install` and by the LazyRAD IDE's
//!   pre-check;
//! * [`recycle`]: when an idle `pkgd` restarts to give its heap back (the one rule, so
//!   the memory tests judge what `pkgd` does);
//! * [`resident`]: the permissions `[entry] resident = true` implies, read by
//!   both [`explain`] and [`rules`];
//! * [`toolgate`]: where a developer tool may come from and when it is
//!   loaded (`docs/devtool-packages-plan.md`, phase T3);
//! * [`hash`]: the FNV-1a hashes the kernel keys policy by.
//!
//! The crate is `no_std` + `alloc` and touches no syscall: [`tree`] reaches
//! the filesystem only through the trait its caller implements.

#![cfg_attr(not(any(test, feature = "fuzz")), no_std)]

extern crate alloc;

pub mod access;
pub mod audit;
pub mod develop;
pub mod docs;
pub mod explain;
pub mod filerules;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
pub mod hash;
pub mod inspect;
pub mod layout;
pub mod provision;
pub mod recycle;
pub mod resident;
pub mod rules;
pub mod shellrole;
pub mod toolgate;
pub mod tree;
pub mod verdicts;
