//! Host tests of the image build modules. `build.rs` cannot run its own tests,
//! so the same source files are included here under the same module names
//! (`crate::os_image` resolves identically in both). Run them with
//! `cargo test -p build-support-tests`.

#![allow(dead_code)]

#[path = "../docs_embed.rs"]
mod docs_embed;
#[path = "../os_disk.rs"]
mod os_disk;
#[path = "../os_image.rs"]
mod os_image;
#[path = "../os_layout.rs"]
mod os_layout;
#[path = "../os_manifest.rs"]
mod os_manifest;

#[cfg(test)]
mod image_tests;
#[cfg(test)]
mod layout_tests;
