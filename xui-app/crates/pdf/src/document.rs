//! Opening a PDF and reading what the viewer lays pages out with.

use std::sync::Arc;

use hayro::hayro_syntax::{LoadPdfError, Pdf};

/// An open PDF. It owns the file's bytes; pages are parsed lazily by hayro.
pub struct Document {
    pub(crate) pdf: Pdf,
}

/// Why a file did not open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenError {
    /// The file is encrypted and the password (possibly the empty one) was
    /// not accepted: ask the user for one.
    Password,
    /// The file is encrypted with a scheme hayro does not implement.
    UnsupportedEncryption,
    /// Not a PDF, or damaged beyond what the xref rebuild repairs.
    Invalid,
    /// A PDF without a single page.
    NoPages,
}

impl OpenError {
    /// A short reason for the user and for `PDF:OPEN:FAIL` evidence.
    pub fn reason(self) -> &'static str {
        match self {
            OpenError::Password => "password required",
            OpenError::UnsupportedEncryption => "unsupported encryption",
            OpenError::Invalid => "not a PDF or damaged",
            OpenError::NoPages => "no pages",
        }
    }
}

/// A page's size in PDF points (1/72 inch) as displayed: crop box, after the
/// page's own `/Rotate`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}

/// The document information dictionary, decoded to text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Info {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
}

impl Document {
    /// Opens `data` with `password` (`""` for an unencrypted file or one with
    /// only an owner password).
    pub fn open(data: Vec<u8>, password: &str) -> Result<Self, OpenError> {
        let pdf = Pdf::new_with_password(Arc::new(data), password).map_err(|e| match e {
            LoadPdfError::Decryption(d) => decryption_error(d),
            LoadPdfError::Invalid => OpenError::Invalid,
        })?;
        if pdf.pages().is_empty() {
            return Err(OpenError::NoPages);
        }
        Ok(Self { pdf })
    }

    pub fn page_count(&self) -> usize {
        self.pdf.pages().len()
    }

    /// The displayed size of page `index` (0-based), or `None` past the end.
    pub fn page_size(&self, index: usize) -> Option<PageSize> {
        let (width, height) = self.pdf.pages().get(index)?.render_dimensions();
        Some(PageSize { width, height })
    }

    /// The PDF version from the file header, as `"1.7"`.
    pub fn version(&self) -> &'static str {
        use hayro::hayro_syntax::PdfVersion::*;
        match self.pdf.version() {
            Pdf10 => "1.0",
            Pdf11 => "1.1",
            Pdf12 => "1.2",
            Pdf13 => "1.3",
            Pdf14 => "1.4",
            Pdf15 => "1.5",
            Pdf16 => "1.6",
            Pdf17 => "1.7",
            Pdf20 => "2.0",
        }
    }

    pub fn info(&self) -> Info {
        let m = self.pdf.metadata();
        Info {
            title: m.title.as_deref().map(decode_text),
            author: m.author.as_deref().map(decode_text),
            subject: m.subject.as_deref().map(decode_text),
            creator: m.creator.as_deref().map(decode_text),
            producer: m.producer.as_deref().map(decode_text),
        }
    }
}

fn decryption_error(e: hayro::hayro_syntax::DecryptionError) -> OpenError {
    use hayro::hayro_syntax::DecryptionError as D;
    match e {
        D::PasswordProtected => OpenError::Password,
        _ => OpenError::UnsupportedEncryption,
    }
}

/// Decodes a PDF text string: UTF-16BE with a BOM, UTF-8 with a BOM (PDF
/// 2.0), else PDFDocEncoding, which matches Latin-1 for printable text.
pub(crate) fn decode_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    bytes.iter().map(|&b| b as char).collect()
}
