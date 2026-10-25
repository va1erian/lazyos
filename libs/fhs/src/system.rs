//! The system tree: everything the image build places outside `/boot`.
//! Read-only to everyone but the build and, from F5, `pkgd`.

/// The system tree. Written by the image build.
pub const SYSTEM: &str = "/system";

/// Every program, by its real lowercase name ([`crate::bin`]).
pub const SYSTEM_BIN: &str = "/system/bin";

/// System configuration ([`crate::etc`]).
pub const SYSTEM_ETC: &str = "/system/etc";

/// Shared objects of Linux programs: the dynamic linker
/// (`ld-musl-x86_64.so.1`) and the libraries it loads. The Linux ABI serves
/// `/lib/<name>`, `/usr/lib/<name>`, `/usr/local/lib/<name>` and
/// `/lib64/<name>` from here when no such file exists, which is where a
/// prebuilt program's `PT_INTERP` and a `dlopen` search look. Written by the
/// image build (`LAZYOS_DYNLINK=1`).
pub const SYSTEM_LIB: &str = "/system/lib";

/// The Rust compiler's target libraries and tools (`LAZYOS_RUSTC=1`,
/// docs/self-hosting-plan.md): `rustc` in [`SYSTEM_BIN`] finds its sysroot,
/// [`SYSTEM`], from the driver library beside this directory, and reads
/// `x86_64-unknown-linux-musl/lib` (the standard library) and
/// `x86_64-unknown-linux-musl/bin/rust-lld` (the linker) under it.
pub const RUSTLIB: &str = "/system/lib/rustlib";

/// The conventional library directories the Linux ABI answers from
/// [`SYSTEM_LIB`] (`/lib/<name>` is `/system/lib/<name>` when no such file
/// exists).
pub const LIB_ALIAS_DIRS: [&str; 4] = ["/lib", "/usr/lib", "/usr/local/lib", "/lib64"];

/// Read-only data: MIME types, samples ([`crate::share`]).
pub const SYSTEM_SHARE: &str = "/system/share";

/// Device firmware the drivers load at run time, one directory per vendor
/// (`rtw88/rtw8822c_fw.bin`), each with the licence text it ships under
/// beside it (`rtw88/LICENCE.rtlwifi_firmware.txt`). Separate files, never
/// compiled into a program (docs/wifi-prerequisites-plan.md 3.8); a driver
/// opens one through `libs/fwload`, which refuses any name outside this tree.
/// Written by the image build from `tools/firmware/fetch.py`'s pinned
/// downloads (`LAZYOS_WIFI=1` images).
pub const FIRMWARE: &str = "/system/share/firmware";

/// Core packages: one `<system_name>.lzp` per app the image ships, which
/// `pkgd` installs into `/apps` at startup (`pkgstore::provision`).
pub const SYSTEM_PACKAGES: &str = "/system/packages";

/// The core package index the image build writes next to them: one
/// `<system_name> <version> <sha256>` line per package, so a boot whose set
/// did not change reads this instead of every archive.
pub const PACKAGES_INDEX: &str = "/system/packages/index";

/// The manifest of every path the image build placed: the only record of what
/// an in-place update may replace or delete (`build_support/os_manifest.rs`).
pub const IMAGE_MANIFEST: &str = "/system/.image-manifest";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directories_nest_under_system() {
        for dir in [
            SYSTEM_BIN,
            SYSTEM_ETC,
            SYSTEM_LIB,
            SYSTEM_SHARE,
            SYSTEM_PACKAGES,
        ] {
            let rest = dir.strip_prefix(SYSTEM).expect(dir);
            assert!(rest.starts_with('/') && !rest[1..].contains('/'), "{dir}");
        }
        assert!(IMAGE_MANIFEST.starts_with(SYSTEM));
        assert_eq!(RUSTLIB.strip_prefix(SYSTEM_LIB), Some("/rustlib"));
        assert!(PACKAGES_INDEX.starts_with(SYSTEM_PACKAGES));
        assert_eq!(FIRMWARE.strip_prefix(SYSTEM_SHARE), Some("/firmware"));
    }
}
