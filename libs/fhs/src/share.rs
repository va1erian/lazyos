//! Read-only data in [`SYSTEM_SHARE`](crate::SYSTEM_SHARE).

/// The MIME type overrides (`/etc/mime.types` syntax: `<mime> <ext>...`), read
/// by `mimed` at boot. Written by the image build.
pub const MIME_TYPES: &str = "/system/share/mime.types";

/// The sample files the image ships (`hello.txt`, `notes.txt`, `testdoc.md`,
/// `pkgdemo.lzp`). Written by the image build. Target (F5): `pkgdemo.lzp` is
/// replaced by real core packages.
pub const SAMPLES: &str = "/system/share/samples";

/// The Docs app's test document, opened by the Docs screenshot session.
pub const TESTDOC: &str = "/system/share/samples/testdoc.md";

/// The sample package (the Counter demo), installed with
/// `pkgctl install /system/share/samples/pkgdemo.lzp`.
pub const PKGDEMO: &str = "/system/share/samples/pkgdemo.lzp";

/// The `lazyrad` sample projects (`LAZYRAD_SAMPLES`), one directory each.
/// Written by the image build (`LAZYOS_LAZYRAD=1` images).
pub const LAZYRAD_SAMPLES: &str = "/system/share/lazyrad";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_lives_in_system_share() {
        for path in [MIME_TYPES, SAMPLES, LAZYRAD_SAMPLES] {
            assert!(path.starts_with(crate::SYSTEM_SHARE), "{path}");
        }
        for path in [TESTDOC, PKGDEMO] {
            assert!(path.starts_with(SAMPLES), "{path}");
        }
    }
}
