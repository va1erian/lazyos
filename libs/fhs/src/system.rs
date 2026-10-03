//! The system tree: everything the image build places outside `/boot`.
//! Read-only to everyone but the build and, from F5, `pkgd`.

/// The system tree. Written by the image build.
pub const SYSTEM: &str = "/system";

/// Every program, by its real lowercase name ([`crate::bin`]).
pub const SYSTEM_BIN: &str = "/system/bin";

/// System configuration ([`crate::etc`]).
pub const SYSTEM_ETC: &str = "/system/etc";

/// Read-only data: MIME types, samples ([`crate::share`]).
pub const SYSTEM_SHARE: &str = "/system/share";

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
        for dir in [SYSTEM_BIN, SYSTEM_ETC, SYSTEM_SHARE, SYSTEM_PACKAGES] {
            let rest = dir.strip_prefix(SYSTEM).expect(dir);
            assert!(rest.starts_with('/') && !rest[1..].contains('/'), "{dir}");
        }
        assert!(IMAGE_MANIFEST.starts_with(SYSTEM));
        assert!(PACKAGES_INDEX.starts_with(SYSTEM_PACKAGES));
    }
}
