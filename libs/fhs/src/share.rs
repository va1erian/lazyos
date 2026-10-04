//! Read-only data in [`SYSTEM_SHARE`](crate::SYSTEM_SHARE).

/// The MIME type overrides (`/etc/mime.types` syntax: `<mime> <ext>...`), read
/// by `mimed` at boot. Written by the image build.
pub const MIME_TYPES: &str = "/system/share/mime.types";

/// The desktop pictures a desktop image ships (`Aurora.jpg`, `Dunes.jpg`,
/// `LazyOS-Night.jpg`, `LazyOS-Green.jpg`; `assets/wallpapers`): Settings
/// lists this directory and LazyShell draws the one `sys/ui/wallpaper` names.
/// Written by the image build.
pub const WALLPAPERS: &str = "/system/share/wallpapers";

/// The sample files the image ships (`hello.txt`, `notes.txt`, `testdoc.md`,
/// `writer-sample.png`, `pkgdemo.lzp`). Written by the image build. Target (F5): `pkgdemo.lzp` is
/// replaced by real core packages.
pub const SAMPLES: &str = "/system/share/samples";

/// The Docs app's test document, opened by the Docs screenshot session.
pub const TESTDOC: &str = "/system/share/samples/testdoc.md";

/// A small picture (200 x 120 PNG) LazyWriter's screenshot session inserts
/// into a document. Written by the image build.
pub const WRITER_SAMPLE_IMAGE: &str = "/system/share/samples/writer-sample.png";

/// The sample package (the Counter demo), installed with
/// `pkgctl install /system/share/samples/pkgdemo.lzp`.
pub const PKGDEMO: &str = "/system/share/samples/pkgdemo.lzp";

/// The Doom package (`org.lazy.doom`, `LAZYOS_DOOM=1` images): a user
/// package, copied to the user's home (the ramfs `/transient` is too small
/// for its 10 MiB) and installed from there with `pkgctl install`.
/// Written by the image build.
pub const DOOM_LZP: &str = "/system/share/samples/doom.lzp";

/// The development-run test package (`org.lazy.test.lrdev`, issue #529): the
/// LazyRAD IDE with `develop = true`, built by `tools/lazyrad/devtest.py` and
/// embedded in `LAZYOS_LAZYRAD=1` images when it was built.
pub const LRDEV_TEST_LZP: &str = "/system/share/samples/lrdev-test.lzp";

/// The LazyRAD MOD player package (`org.lazy.modplayer`,
/// `LAZYOS_MODPLAYER=1` images): a user package, copied to the user's home
/// and installed from there with `pkgctl install`, like [`DOOM_LZP`].
/// Written by the image build.
pub const MODPLAYER_LZP: &str = "/system/share/samples/modplayer.lzp";

/// The `lazyrad` sample projects (`LAZYRAD_SAMPLES`), one directory each.
/// Written by the image build (`LAZYOS_LAZYRAD=1` images).
pub const LAZYRAD_SAMPLES: &str = "/system/share/lazyrad";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_lives_in_system_share() {
        for path in [MIME_TYPES, WALLPAPERS, SAMPLES, LAZYRAD_SAMPLES] {
            assert!(path.starts_with(crate::SYSTEM_SHARE), "{path}");
        }
        for path in [
            TESTDOC,
            WRITER_SAMPLE_IMAGE,
            PKGDEMO,
            DOOM_LZP,
            LRDEV_TEST_LZP,
            MODPLAYER_LZP,
        ] {
            assert!(path.starts_with(SAMPLES), "{path}");
        }
    }
}
