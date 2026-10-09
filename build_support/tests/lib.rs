//! Host tests of the image build modules. `build.rs` cannot run its own tests,
//! so the same source files are included here under the same module names
//! (`crate::os_image` resolves identically in both). Run them with
//! `cargo test -p build-support-tests`.

#![allow(dead_code)]

#[path = "../accounts_seed.rs"]
mod accounts_seed;
#[path = "../assets_embed.rs"]
mod assets_embed;
#[path = "../ca_bundle.rs"]
mod ca_bundle;
#[path = "../core_packages.rs"]
mod core_packages;
#[path = "../docs_embed.rs"]
mod docs_embed;
#[path = "../hosts_embed.rs"]
mod hosts_embed;
#[path = "../lazyweb_embed.rs"]
mod lazyweb_embed;
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
#[path = "../pictures_embed.rs"]
mod pictures_embed;
#[path = "../samples_embed.rs"]
mod samples_embed;
#[path = "../tls_embed.rs"]
mod tls_embed;
#[path = "../usb_fat.rs"]
mod usb_fat;
#[path = "../usb_ramdisk.rs"]
mod usb_ramdisk;
#[path = "../usb_stick.rs"]
mod usb_stick;

#[cfg(test)]
mod accounts_tests;
#[cfg(test)]
mod assets_tests;
#[cfg(test)]
mod dbgd_tests;
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
mod lazyweb_tests;
#[cfg(test)]
mod limits_tests;
#[cfg(test)]
mod pictures_tests;
#[cfg(test)]
mod recover_tests;
#[cfg(test)]
mod samples_tests;
#[cfg(test)]
mod state_tests;
#[cfg(test)]
mod tls_files_tests;
#[cfg(test)]
mod usb_tests;
#[cfg(test)]
mod wallpapers_tests;
