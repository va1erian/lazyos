//! The sample files every image ships in `/system/share/samples`: the
//! generated ones (`samples_embed`) and the binary ones (assets).

use crate::os_image::{OsFiles, Sink};

fn samples() -> OsFiles {
    let mut files = OsFiles::default();
    crate::samples_embed::embed(&mut files);
    for (dest, asset) in crate::assets_tests::checked_in(false) {
        if dest.starts_with(fhs::share::SAMPLES) {
            files.add_file(&dest, asset.file);
        }
    }
    files
}

fn bytes(files: &OsFiles, path: &str) -> Vec<u8> {
    let file = files
        .files()
        .into_iter()
        .find(|file| file.path == path)
        .unwrap_or_else(|| panic!("{path} is not shipped"));
    file.source.read(path).unwrap()
}

#[test]
fn every_sample_lands_in_the_samples_directory() {
    let files = samples();
    // The samples, and the Terminal's terminfo entry `samples_embed` also makes.
    assert_eq!(files.len(), 10);
    for file in files.files() {
        let ok = file.path.starts_with(fhs::share::SAMPLES)
            || file.path == fhs::share::TERMINFO_XTERM_256COLOR;
        assert!(ok, "{}", file.path);
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

#[test]
fn the_pdf_sample_is_a_pdf() {
    // The PDF Viewer's screenshot session opens it (docs/pdf-reader-plan.md).
    let files = samples();
    let pdf = bytes(&files, fhs::share::PDF_SAMPLE);
    assert!(pdf.starts_with(b"%PDF-"), "{:?}", &pdf[..pdf.len().min(8)]);
    assert!(
        pdf.len() < 256 * 1024,
        "keep the sample small: {} bytes",
        pdf.len()
    );
}

#[test]
fn the_video_sample_is_a_small_avi() {
    // The Video Player's screenshot session opens it (docs/video-player-plan.md).
    let avi = bytes(&samples(), fhs::share::VIDEO_SAMPLE);
    assert!(avi.starts_with(b"RIFF"), "{:?}", &avi[..avi.len().min(8)]);
    assert_eq!(&avi[8..12], b"AVI ");
    assert!(
        avi.len() < 1024 * 1024,
        "keep the sample small: {} bytes",
        avi.len()
    );
}

#[test]
fn the_nes_sample_is_a_small_cartridge() {
    // The NES Player's welcome screen offers it and its screenshot session
    // plays it (docs/nes-player.md); `tools/nes/make_sample.py` writes it.
    let rom = bytes(&samples(), fhs::share::NES_SAMPLE);
    assert_eq!(&rom[..4], b"NES\x1a", "{:?}", &rom[..rom.len().min(4)]);
    assert_eq!((rom[4], rom[5]), (2, 1), "32 KiB PRG + 8 KiB CHR");
    assert_eq!(rom.len(), 16 + 32 * 1024 + 8 * 1024);
    assert!(
        rom.len() < 64 * 1024,
        "keep the sample small: {} bytes",
        rom.len()
    );
}

#[test]
fn the_archiver_samples_are_small_archives() {
    // The Archiver's screenshot session opens them (docs/archiver-plan.md).
    let files = samples();
    let zip = bytes(&files, fhs::share::ARCHIVER_SAMPLE_ZIP);
    assert!(zip.starts_with(b"PK"));
    let sevenz = bytes(&files, fhs::share::ARCHIVER_SAMPLE_7Z);
    assert!(sevenz.starts_with(&[b'7', b'z', 0xbc, 0xaf, 0x27, 0x1c]));
    assert!(
        zip.len() < 8192 && sevenz.len() < 8192,
        "keep the samples tiny"
    );
}

#[test]
fn the_terminal_terminfo_entry_is_compiled_into_the_image() {
    let compiled = bytes(&samples(), fhs::share::TERMINFO_XTERM_256COLOR);
    assert!(fhs::share::TERMINFO_XTERM_256COLOR.starts_with(fhs::share::TERMINFO));
    let entry = terminfo::read_compiled(&compiled).expect("a valid legacy entry");
    assert!(entry.names.starts_with("xterm-256color|"));
    assert_eq!(entry.number("colors"), Some(256));
    // Reproducible: the same bytes every build.
    assert_eq!(
        bytes(&samples(), fhs::share::TERMINFO_XTERM_256COLOR),
        compiled
    );
}
