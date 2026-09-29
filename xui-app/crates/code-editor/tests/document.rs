//! File-model tests: line-ending and BOM round-trips, encoding failures, the
//! size cap, atomic saves and symlinks. They use a unique directory under the
//! system temp directory and clean it up, so they never touch real user files.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use xui_code_editor::document::{Document, DocumentError, LineEnding};

/// A unique temporary directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("xui-editor-doc-{tag}-{}-{n}", std::process::id()));
        fs::create_dir_all(&path).expect("temp dir");
        TempDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Creates a symlink to `original` at `link`, cross-platform.
fn symlink(original: &Path, link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(original, link)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(original, link)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (original, link);
        Err(io::Error::new(io::ErrorKind::Unsupported, "no symlinks"))
    }
}

#[test]
fn an_lf_file_round_trips_byte_for_byte() {
    let dir = TempDir::new("lf");
    let file = dir.file("a.txt");
    fs::write(&file, "one\ntwo\n").expect("write");

    let (mut document, text) = Document::load(&file).expect("load");
    assert_eq!(text, "one\ntwo\n");
    assert_eq!(document.line_ending(), LineEnding::Lf);
    assert!(!document.has_bom());
    assert!(!document.is_dirty(0), "a fresh load is clean");

    document.save(&text, 0).expect("save");
    assert_eq!(fs::read(&file).expect("read"), b"one\ntwo\n");
}

#[test]
fn a_crlf_file_round_trips_byte_for_byte() {
    let dir = TempDir::new("crlf");
    let file = dir.file("a.txt");
    fs::write(&file, b"one\r\ntwo\r\n").expect("write");

    let (mut document, text) = Document::load(&file).expect("load");
    assert_eq!(text, "one\ntwo\n", "the buffer is normalised to LF");
    assert_eq!(document.line_ending(), LineEnding::CrLf);

    document.save(&text, 0).expect("save");
    assert_eq!(fs::read(&file).expect("read"), b"one\r\ntwo\r\n");
}

#[test]
fn a_utf8_bom_is_stripped_on_load_and_written_back() {
    let dir = TempDir::new("bom");
    let file = dir.file("a.txt");
    fs::write(&file, b"\xEF\xBB\xBFhello\n").expect("write");

    let (mut document, text) = Document::load(&file).expect("load");
    assert_eq!(text, "hello\n", "the BOM is not part of the text");
    assert!(document.has_bom());

    document.save(&text, 0).expect("save");
    assert_eq!(fs::read(&file).expect("read"), b"\xEF\xBB\xBFhello\n");
}

#[test]
fn mixed_endings_are_saved_with_the_majority_ending() {
    let dir = TempDir::new("mixed");
    let file = dir.file("a.txt");
    fs::write(&file, b"a\r\nb\nc\n").expect("write");

    let (mut document, text) = Document::load(&file).expect("load");
    assert_eq!(document.line_ending(), LineEnding::Lf, "LF is the majority");
    assert_eq!(text, "a\nb\nc\n");
    document.save(&text, 0).expect("save");
    assert_eq!(fs::read(&file).expect("read"), b"a\nb\nc\n");
}

#[test]
fn an_empty_file_round_trips() {
    let dir = TempDir::new("empty");
    let file = dir.file("a.txt");
    fs::write(&file, b"").expect("write");

    let (mut document, text) = Document::load(&file).expect("load");
    assert!(text.is_empty());
    document.save(&text, 0).expect("save");
    assert_eq!(fs::read(&file).expect("read"), b"");
}

#[test]
fn a_file_without_a_trailing_newline_round_trips() {
    let dir = TempDir::new("no-trailing");
    let file = dir.file("a.txt");
    fs::write(&file, b"tail").expect("write");

    let (mut document, text) = Document::load(&file).expect("load");
    assert_eq!(text, "tail");
    document.save(&text, 0).expect("save");
    assert_eq!(fs::read(&file).expect("read"), b"tail");
}

#[test]
fn invalid_utf8_is_rejected_with_the_offset() {
    let dir = TempDir::new("utf8");
    let file = dir.file("a.txt");
    fs::write(&file, b"ok\xFFbad").expect("write");

    let error = Document::load(&file).expect_err("invalid utf-8");
    match error {
        DocumentError::InvalidUtf8 { offset, .. } => assert_eq!(offset, 2),
        other => panic!("expected InvalidUtf8, got {other}"),
    }
}

#[test]
fn a_directory_is_not_a_file() {
    let dir = TempDir::new("dir");
    let error = Document::load(dir.path()).expect_err("a directory is not loadable");
    assert!(matches!(error, DocumentError::NotAFile { .. }));
}

#[test]
fn a_file_over_the_cap_is_rejected() {
    let dir = TempDir::new("large");
    let file = dir.file("a.txt");
    fs::write(&file, b"hello").expect("write");

    let error = Document::load_with_limit(&file, 2).expect_err("over the cap");
    match error {
        DocumentError::TooLarge { size, limit, .. } => {
            assert_eq!(size, 5);
            assert_eq!(limit, 2);
        }
        other => panic!("expected TooLarge, got {other}"),
    }
}

#[test]
fn saving_an_untitled_document_needs_save_as() {
    let mut document = Document::untitled();
    assert!(matches!(
        document.save("text", 0),
        Err(DocumentError::NoPath)
    ));
}

#[test]
fn a_failed_save_leaves_the_original_file_intact() {
    let dir = TempDir::new("failed");
    let file = dir.file("a.txt");
    fs::write(&file, b"original").expect("write");

    let (mut document, _text) = Document::load(&file).expect("load");
    let missing = dir.path().join("no-such-dir").join("b.txt");
    assert!(document.save_as(&missing, "new", 1).is_err());
    assert!(!document.is_dirty(0), "the saved revision did not move");
    assert!(document.is_dirty(1), "a later revision is still dirty");
    assert_eq!(fs::read(&file).expect("read"), b"original");
    assert!(
        !missing.exists(),
        "the failed save must not create its target"
    );
}

#[test]
fn a_successful_save_leaves_no_temp_file_behind() {
    let dir = TempDir::new("atomic");
    let file = dir.file("a.txt");
    fs::write(&file, b"one").expect("write");

    let (mut document, _text) = Document::load(&file).expect("load");
    document.save_as(&file, "two", 3).expect("save");

    let names: Vec<String> = fs::read_dir(dir.path())
        .expect("read dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(names, vec!["a.txt".to_string()], "only the target remains");
    assert_eq!(fs::read(&file).expect("read"), b"two");
}

#[test]
fn saving_through_a_symlink_is_refused_and_leaves_the_target_alone() {
    let dir = TempDir::new("symlink");
    let target = dir.file("real.txt");
    let link = dir.file("link.txt");
    fs::write(&target, b"old").expect("write target");
    if symlink(&target, &link).is_err() {
        // Windows without developer mode refuses symlinks; skip, do not fail.
        return;
    }

    let mut document = Document::untitled();
    assert!(
        document.save_as(&link, "new", 0).is_err(),
        "a symlink target must be refused"
    );

    assert!(
        fs::symlink_metadata(&link)
            .expect("link metadata")
            .file_type()
            .is_symlink(),
        "the link is still a link"
    );
    assert_eq!(
        fs::read(&target).expect("read target"),
        b"old",
        "the link's target is untouched"
    );
    assert_eq!(
        document.display_name(),
        "Untitled",
        "a refused save_as must not adopt the path"
    );
}

#[test]
fn the_display_name_is_untitled_with_no_path() {
    let document = Document::untitled();
    assert_eq!(document.display_name(), "Untitled");
    let dir = TempDir::new("name");
    let file = dir.file("notes.txt");
    fs::write(&file, b"x").expect("write");
    let (document, _text) = Document::load(&file).expect("load");
    assert_eq!(document.display_name(), "notes.txt");
}
