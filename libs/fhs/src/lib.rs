//! Every well-known path and boot-volume file name, defined once.
//!
//! The filesystem overhaul (`docs/filesystem-plan.md`) moves the FAT plus
//! `/data` layout to an ext2 OS volume with a real tree. Each phase changes a
//! constant here instead of chasing string literals through the kernel,
//! services, apps and `lazyrad`. Each constant documents what lives there, who
//! writes it and, where a later phase moves it, the value it will take.
//!
//! Since F3 every program lives at its real lowercase name in [`SYSTEM_BIN`]
//! ([`bin`]), system configuration in [`SYSTEM_ETC`] ([`etc`]) and read-only
//! data in [`SYSTEM_SHARE`] ([`share`]); the FAT `/boot` holds only the kernel
//! and `lazyos.cfg` ([`boot`]).
//!
//! `tools/fhs/check_literals.py` fails CI when such a literal reappears
//! outside this crate. Linux ABI synthetic paths (`/dev`, `/proc`, `/bin`) are
//! ABI concepts, not LazyOS layout, and stay in `process/linux`.
//!
//! No dependencies and no `alloc` unless the `alloc` feature is on.

#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;

pub mod bin;
pub mod boot;
pub mod docs;
pub mod etc;
#[cfg(feature = "alloc")]
mod install;
pub mod mount;
pub mod share;
pub mod state;
pub mod system;

#[cfg(feature = "alloc")]
pub use install::{app_data_dir, home_of, install_path};
pub use system::{SYSTEM, SYSTEM_BIN, SYSTEM_ETC, SYSTEM_PACKAGES, SYSTEM_SHARE};
