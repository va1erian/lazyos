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

/// Core packages. Empty until F5.
pub const SYSTEM_PACKAGES: &str = "/system/packages";

/// The manifest of every path the image build placed: the only record of what
/// an in-place update may replace or delete (`build_support/os_manifest.rs`).
pub const IMAGE_MANIFEST: &str = "/system/.image-manifest";

/// The list of optional apps the build embedded, one `/system/bin` path per
/// line, optionally followed by `autostart`; `init` reads it at boot. Target
/// (F5): removed, the apps become packages.
pub const XAPPS_LST: &str = "/system/etc/xapps.lst";

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
        assert!(XAPPS_LST.starts_with(SYSTEM_ETC));
    }
}
