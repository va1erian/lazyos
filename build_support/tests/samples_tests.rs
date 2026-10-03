//! The sample files every image ships in `/system/share/samples`.

use crate::os_image::{OsFiles, Source};

fn samples() -> OsFiles {
    let mut files = OsFiles::default();
    crate::samples_embed::embed(&mut files);
    files
}

fn bytes(files: &OsFiles, path: &str) -> Vec<u8> {
    let file = files
        .files()
        .into_iter()
        .find(|file| file.path == path)
        .unwrap_or_else(|| panic!("{path} is not shipped"));
    match file.source {
        Source::Bytes(bytes) => bytes,
        Source::Path(path) => std::fs::read(path).unwrap(),
    }
}

#[test]
fn every_sample_lands_in_the_samples_directory() {
    let files = samples();
    assert_eq!(files.len(), 4);
    for file in files.files() {
        assert!(file.path.starts_with(fhs::share::SAMPLES), "{}", file.path);
    }
    assert!(!bytes(&files, fhs::share::TESTDOC).is_empty());
}

#[test]
fn the_writer_sample_is_a_small_png() {
    // LazyWriter's screenshot session inserts it (issue #533); the app fits
    // pictures to 360 dip, so a 200 x 120 one goes in at its own size.
    let png = bytes(&samples(), fhs::share::WRITER_SAMPLE_IMAGE);
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(&png[12..16], b"IHDR");
    let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
    assert_eq!((width, height), (200, 120));
    assert!(
        png.len() < 4096,
        "keep the sample tiny: {} bytes",
        png.len()
    );
}
