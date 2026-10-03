//! Host tests for the file logic: open errors, round trips, export layout
//! and picture loading.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use xui_rich_text::DocPos;
use xui_rich_text::format::{FormatError, to_json};
use xui_rich_text::model::{Document, EditOp};

use super::*;

/// A unique folder under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("lazywriter-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn picture(w: u32, h: u32) -> Image {
    Image::from_rgba(w, h, vec![200; (w * h * 4) as usize]).unwrap()
}

fn document_with_picture() -> Document {
    let mut doc = Document::from_plain_text("Before\nAfter");
    let image = image_from_bytes(&picture(4, 2).encode_png().unwrap(), "dot".into()).unwrap();
    doc.apply(EditOp::InsertObject {
        at: DocPos::new(0, 6),
        object: image,
    })
    .unwrap();
    doc
}

#[test]
fn bad_json_is_a_syntax_error() {
    let error = parse_document(b"{ not json", false).unwrap_err();
    assert!(matches!(error, OpenError::Format(FormatError::Syntax(_))));
    assert!(error.to_string().starts_with("not a rich-text file"));
}

#[test]
fn another_version_is_refused_with_its_number() {
    let json = to_json(&Document::from_plain_text("hi")).replace("\"version\":1", "\"version\":99");
    let error = parse_document(json.as_bytes(), false).unwrap_err();
    assert!(matches!(error, OpenError::Format(FormatError::Version(99))));
    assert_eq!(error.to_string(), "unsupported file version 99");
}

#[test]
fn binary_data_is_not_a_document() {
    let error = parse_document(&[0xFF, 0xFE, 0x00, 0x80], false).unwrap_err();
    assert!(matches!(error, OpenError::NotText));
}

#[test]
fn plain_text_opens_even_when_not_utf8() {
    let doc = parse_document(b"one\ntwo \xFF", true).unwrap();
    assert_eq!(doc.paragraphs().len(), 2);
}

#[test]
fn an_oversize_file_is_refused_unread() {
    let dir = TempDir::new("oversize");
    let path = dir.file("huge.lzw");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(MAX_DOCUMENT_BYTES + 1).unwrap();
    let error = open_document(&path).unwrap_err();
    assert!(matches!(
        error,
        OpenError::Read(ReadError::TooLarge(MAX_DOCUMENT_BYTES))
    ));
}

#[test]
fn a_missing_file_is_a_read_error() {
    let error = open_document(Path::new("/nonexistent/lazywriter/x.lzw")).unwrap_err();
    assert!(matches!(error, OpenError::Read(ReadError::Io(_))));
}

#[test]
fn a_saved_document_opens_again_with_its_picture() {
    let dir = TempDir::new("roundtrip");
    let host = Host::std(&dir.0);
    let path = dir.file("doc.lzw");
    let doc = document_with_picture();
    save_document(&host, &path, &doc).unwrap();
    let back = open_document(&path).unwrap();
    assert_eq!(back.to_plain_text(), doc.to_plain_text());
    assert_eq!(back.objects().len(), 1);
}

#[test]
fn a_txt_file_opens_as_plain_text() {
    let dir = TempDir::new("txt");
    let path = dir.file("notes.txt");
    std::fs::write(&path, "alpha\nbeta").unwrap();
    let doc = open_document(&path).unwrap();
    assert_eq!(doc.paragraphs().len(), 2);
    assert_eq!(word_count(&doc), 2);
}

#[test]
fn export_writes_markdown_and_an_images_folder() {
    let dir = TempDir::new("export");
    let host = Host::std(&dir.0);
    let md = dir.file("trip.md");
    export_markdown(&host, &md, &document_with_picture()).unwrap();
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.contains("trip_images/1.png"), "{text}");
    let png = std::fs::read(dir.file("trip_images/1.png")).unwrap();
    assert_eq!(probe::dimensions(&png), Some((4, 2)));
}

#[test]
fn export_without_pictures_makes_no_folder() {
    let dir = TempDir::new("export-plain");
    let host = Host::std(&dir.0);
    let md = dir.file("plain.md");
    export_markdown(&host, &md, &Document::from_plain_text("Just words")).unwrap();
    assert!(std::fs::read_to_string(&md).unwrap().contains("Just words"));
    assert!(!dir.file("plain_images").exists());
}

#[test]
fn a_failed_write_is_reported() {
    let dir = TempDir::new("fail");
    let mut host = Host::std(&dir.0);
    host.write = std::rc::Rc::new(|_, _| Err("disk full".to_owned()));
    let error = save_document(&host, &dir.file("x.lzw"), &Document::new()).unwrap_err();
    assert_eq!(error, "disk full");
    let error = export_markdown(&host, &dir.file("x.md"), &document_with_picture()).unwrap_err();
    assert_eq!(error, "disk full");
}

#[test]
fn a_wide_picture_is_fitted_to_the_column() {
    let image = image_from_bytes(&picture(720, 100).encode_png().unwrap(), "w".into()).unwrap();
    assert_eq!(image.size, (Dip(360.0), Dip(50.0)));
    let image = image_from_bytes(&picture(100, 40).encode_png().unwrap(), "n".into()).unwrap();
    assert_eq!(
        image.size,
        (Dip(100.0), Dip(40.0)),
        "small pictures keep their size"
    );
}

#[test]
fn a_picture_that_is_not_one_is_refused() {
    assert!(image_from_bytes(b"GIF89a", String::new()).is_err());
    let mut png = picture(2, 2).encode_png().unwrap();
    png.truncate(40);
    assert!(image_from_bytes(&png, String::new()).is_err(), "truncated");
}

#[test]
fn a_picture_bomb_is_refused_before_decoding() {
    // A PNG header claiming 100000 x 100000 pixels, with no data behind it.
    let mut png = picture(1, 1).encode_png().unwrap();
    png[16..20].copy_from_slice(&100_000u32.to_be_bytes());
    png[20..24].copy_from_slice(&100_000u32.to_be_bytes());
    let error = image_from_bytes(&png, String::new()).unwrap_err();
    assert!(error.contains("100000 x 100000"), "{error}");
}

#[test]
fn an_oversize_picture_file_is_refused() {
    let dir = TempDir::new("bigpic");
    let path = dir.file("big.png");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(MAX_IMAGE_BYTES + 1)
        .unwrap();
    let error = load_image(&path).unwrap_err();
    assert_eq!(error, "the file is larger than 16 MiB");
}
