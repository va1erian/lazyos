//! Host tests of the image build modules. `build.rs` cannot run its own tests,
//! so the same source files are included here under the same module names
//! (`crate::os_image` resolves identically in both). Run them with
//! `cargo test -p build-support-tests`.

#![allow(dead_code)]

#[path = "../ca_bundle.rs"]
mod ca_bundle;
#[path = "../hosts_embed.rs"]
mod hosts_embed;
#[path = "../tls_embed.rs"]
mod tls_embed;
#[path = "../core_packages.rs"]
mod core_packages;
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
#[path = "../os_recover.rs"]
mod os_recover;
#[path = "../samples_embed.rs"]
mod samples_embed;

#[cfg(test)]
mod f3_layout_tests;
#[cfg(test)]
mod f4_layout_tests;
#[cfg(test)]
mod f5_layout_tests;
#[cfg(test)]
mod image_tests;
#[cfg(test)]
mod layout_tests;
#[cfg(test)]
mod limits_tests;
#[cfg(test)]
mod recover_tests;
#[cfg(test)]
mod samples_tests;
#[cfg(test)]
mod tls_files_tests;
