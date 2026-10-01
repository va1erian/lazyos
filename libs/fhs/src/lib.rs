//! Every well-known path and boot-volume file name, defined once.
//!
//! The filesystem overhaul (`docs/filesystem-plan.md`) moves the FAT plus
//! `/data` layout to an ext2 OS volume with a real tree. Phases F1 to F4 then
//! change a constant here instead of chasing string literals through the
//! kernel, services, apps and `lazyrad`. Each constant documents what lives
//! there, who writes it and the value it takes in the target tree. Today every
//! constant keeps the value the code used before this crate existed.
//!
//! `tools/fhs/check_literals.py` fails CI when such a literal reappears
//! outside this crate. Linux ABI synthetic paths (`/dev`, `/proc`, `/etc`,
//! `/bin`) are ABI concepts, not LazyOS layout, and stay in `process/linux`.
//!
//! No dependencies and no `alloc` unless the `alloc` feature is on.

#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;

pub mod boot;
pub mod docs;
#[cfg(feature = "alloc")]
mod install;
pub mod mount;
pub mod state;

#[cfg(feature = "alloc")]
pub use install::install_path;
