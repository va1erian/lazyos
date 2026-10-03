#![forbid(unsafe_code)]

//! Pure file-name and size rules: the native extension, the names the Save
//! and Export dialogs suggest, the export's image folder and the read caps.
//!
//! Nothing here touches the filesystem, so the rules are host-tested as plain
//! functions.

use std::io::Read;
use std::path::{Path, PathBuf};

/// LazyWriter's own extension.
pub const EXTENSION: &str = "lzw";
/// The extension a Markdown export gets.
pub const MARKDOWN_EXTENSION: &str = "md";
/// The image extensions Insert image offers.
pub const IMAGE_EXTENSIONS: [&str; 3] = ["png", "jpg", "jpeg"];
/// The plain-text extension Open accepts besides `.lzw`.
pub const TEXT_EXTENSION: &str = "txt";

/// The largest document Open reads, matching the Editor.
pub const MAX_DOCUMENT_BYTES: u64 = 32 * 1024 * 1024;
/// The largest picture Insert image reads.
pub const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;

/// The name an untitled document is offered under.
const UNTITLED_STEM: &str = "Untitled";

/// The stem of `path`'s file name, or `Untitled` when there is no usable one.
pub fn stem(path: Option<&Path>) -> String {
    path.and_then(Path::file_stem)
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| UNTITLED_STEM.to_owned())
}

/// The name the status bar and title show: the file name, or `Untitled`.
pub fn display_name(path: Option<&Path>) -> String {
    path.and_then(Path::file_name)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| UNTITLED_STEM.to_owned())
}

/// The name Save As suggests: the current name with the `.lzw` extension
/// (an opened `notes.txt` is offered as `notes.lzw`).
pub fn suggested_document_name(path: Option<&Path>) -> String {
    format!("{}.{EXTENSION}", stem(path))
}

/// The name Export suggests: the document's stem with `.md`.
pub fn suggested_export_name(path: Option<&Path>) -> String {
    format!("{}.{MARKDOWN_EXTENSION}", stem(path))
}

/// `path` with the `.lzw` extension added when the user typed none, so a
/// saved document always opens from Files. Another extension the user typed
/// is kept: it was deliberate.
pub fn with_document_extension(path: PathBuf) -> PathBuf {
    if path.extension().is_some() {
        path
    } else {
        path.with_extension(EXTENSION)
    }
}

/// `path` with `.md` added when the user typed no extension.
pub fn with_markdown_extension(path: PathBuf) -> PathBuf {
    if path.extension().is_some() {
        path
    } else {
        path.with_extension(MARKDOWN_EXTENSION)
    }
}

/// Whether `path` names a plain-text file (opened as text, not JSON).
pub fn is_plain_text(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case(TEXT_EXTENSION))
}

/// The folder name an export of `markdown` puts its images in:
/// `<stem>_images`, beside the `.md`.
pub fn images_folder_name(markdown: &Path) -> String {
    let stem = markdown
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "document".to_owned());
    format!("{stem}_images")
}

/// The folder an export of `markdown` puts its images in.
pub fn images_folder(markdown: &Path) -> PathBuf {
    let parent = markdown.parent().unwrap_or(Path::new("."));
    parent.join(images_folder_name(markdown))
}

/// The file name of the `index`th exported image (1-based) and the relative
/// link the Markdown uses for it.
pub fn image_entry(markdown: &Path, index: usize) -> (String, String) {
    let name = format!("{index}.png");
    let link = format!("{}/{name}", images_folder_name(markdown));
    (name, link)
}

/// Why a capped read failed.
#[derive(Debug)]
pub enum ReadError {
    /// The file could not be read.
    Io(std::io::Error),
    /// The file is larger than the cap, in bytes.
    TooLarge(u64),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Io(error) => write!(f, "{error}"),
            ReadError::TooLarge(cap) => {
                write!(f, "the file is larger than {} MiB", cap / (1024 * 1024))
            }
        }
    }
}

/// Reads at most `cap` bytes from `reader`; one byte more is an error, so an
/// oversize file is refused without reading all of it.
pub fn read_capped(reader: impl Read, cap: u64) -> Result<Vec<u8>, ReadError> {
    let mut bytes = Vec::new();
    reader
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(ReadError::Io)?;
    if bytes.len() as u64 > cap {
        return Err(ReadError::TooLarge(cap));
    }
    Ok(bytes)
}

/// Reads the file at `path`, refusing anything over `cap` bytes.
pub fn read_file_capped(path: &Path, cap: u64) -> Result<Vec<u8>, ReadError> {
    let file = std::fs::File::open(path).map_err(ReadError::Io)?;
    read_capped(file, cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_untitled_document_suggests_untitled_lzw() {
        assert_eq!(suggested_document_name(None), "Untitled.lzw");
        assert_eq!(suggested_export_name(None), "Untitled.md");
        assert_eq!(display_name(None), "Untitled");
    }

    #[test]
    fn a_named_document_keeps_its_stem() {
        let path = Path::new("/home/a/notes.txt");
        assert_eq!(suggested_document_name(Some(path)), "notes.lzw");
        assert_eq!(suggested_export_name(Some(path)), "notes.md");
        assert_eq!(display_name(Some(path)), "notes.txt");
        let path = Path::new("/home/a/report.lzw");
        assert_eq!(suggested_document_name(Some(path)), "report.lzw");
    }

    #[test]
    fn a_save_without_an_extension_gets_lzw() {
        assert_eq!(
            with_document_extension(PathBuf::from("/t/letter")),
            PathBuf::from("/t/letter.lzw")
        );
        assert_eq!(
            with_document_extension(PathBuf::from("/t/letter.lzw")),
            PathBuf::from("/t/letter.lzw")
        );
        assert_eq!(
            with_document_extension(PathBuf::from("/t/letter.json")),
            PathBuf::from("/t/letter.json"),
            "an extension the user typed is kept"
        );
        assert_eq!(
            with_markdown_extension(PathBuf::from("/t/out")),
            PathBuf::from("/t/out.md")
        );
    }

    #[test]
    fn plain_text_is_recognised_by_extension() {
        assert!(is_plain_text(Path::new("/t/a.txt")));
        assert!(is_plain_text(Path::new("/t/A.TXT")));
        assert!(!is_plain_text(Path::new("/t/a.lzw")));
        assert!(!is_plain_text(Path::new("/t/txt")));
    }

    #[test]
    fn export_images_go_in_a_folder_named_after_the_markdown() {
        let md = Path::new("/home/a/trip.md");
        assert_eq!(images_folder_name(md), "trip_images");
        assert_eq!(images_folder(md), PathBuf::from("/home/a/trip_images"));
        assert_eq!(
            image_entry(md, 1),
            ("1.png".to_owned(), "trip_images/1.png".to_owned())
        );
        assert_eq!(image_entry(md, 12).1, "trip_images/12.png");
    }

    #[test]
    fn an_export_without_a_stem_uses_document_images() {
        assert_eq!(images_folder_name(Path::new("/")), "document_images");
    }

    #[test]
    fn the_caps_match_the_issue() {
        assert_eq!(MAX_DOCUMENT_BYTES, 32 * 1024 * 1024);
        assert_eq!(MAX_IMAGE_BYTES, 16 * 1024 * 1024);
    }

    #[test]
    fn a_read_at_the_cap_succeeds_and_one_byte_more_fails() {
        let data = [7u8; 64];
        assert_eq!(read_capped(&data[..], 64).unwrap().len(), 64);
        let error = read_capped(&data[..], 63).unwrap_err();
        assert!(matches!(error, ReadError::TooLarge(63)));
    }

    #[test]
    fn an_oversize_read_stops_after_the_cap() {
        // An endless reader: the cap, not the end of the data, stops the read.
        let error = read_capped(std::io::repeat(1), 1024).unwrap_err();
        assert!(matches!(error, ReadError::TooLarge(1024)));
        assert_eq!(
            ReadError::TooLarge(MAX_DOCUMENT_BYTES).to_string(),
            "the file is larger than 32 MiB"
        );
    }
}
