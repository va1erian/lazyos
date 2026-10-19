//! `lazyos-install`: put LazyOS on two partitions of a disk Linux already
//! owns and make it an entry of that Linux's GRUB
//! (docs/grub-install-plan.md, I2).
//!
//! The stick is the payload (decision 2): the loader, the kernel and the
//! ramdisk whose second partition is the OS volume. The OS volume is
//! formatted, populated and updated by the build's own modules, included
//! below by path exactly as `build_support/tests` includes them, so the
//! manifest rules exist once (decision 5).
//!
//! * [`args`]: the command line.
//! * [`plan`]: pure decisions: partition validation, `lazyos.cfg`, the GRUB
//!   entry, which GRUB tool to run. Host-tested, no root.
//! * [`payload`]: the stick's files.
//! * [`populate`]: the system and home volumes.
//! * [`esp`]: the ESP files, in a mounted ESP or a disk image's.
//! * [`grub`]: the snippet or `custom.cfg`, regenerating `grub.cfg`.
//! * [`target`]: finding the partitions (Linux block devices or an image).
//! * [`verify`]: the checker and the ESP hashes.
//! * [`run`]: `install`, `update`, `uninstall`, `status`.
//! * [`testdisk`]: Linux-shaped disk images for the tests and the harness.

// The shared build modules carry what only the image build uses.
#![allow(dead_code)]

#[path = "../../../build_support/os_disk.rs"]
mod os_disk;
#[path = "../../../build_support/os_image.rs"]
mod os_image;
#[path = "../../../build_support/os_layout.rs"]
mod os_layout;
#[path = "../../../build_support/os_manifest.rs"]
mod os_manifest;
#[path = "../../../build_support/os_recover.rs"]
mod os_recover;
#[path = "../../../build_support/usb_ramdisk.rs"]
mod usb_ramdisk;
#[path = "../../../build_support/usb_stick.rs"]
mod usb_stick;

pub mod args;
pub mod esp;
pub mod grub;
pub mod grubenv;
#[cfg(unix)]
pub mod linux;
pub mod payload;
pub mod plan;
pub mod populate;
pub mod probe;
pub mod run;
pub mod target;
pub mod testdisk;
pub mod verify;

#[cfg(test)]
mod tests;

pub use os_disk::{FileIo, SECTOR};
pub use os_image::format_uuid;
