#![forbid(unsafe_code)]

//! A UI-free file model for an editor: line endings, a UTF-8 BOM, the path of
//! the file on disk and the buffer revision the file was last saved at.
//!
//! It is deliberately free of xui and of the event loop, so the rules are
//! unit-tested directly. The text is normalised to `\n` in memory and converted
//! back to the document's line ending when it is written, so a file with only
//! CRLF line endings round-trips byte for byte.
//!
//! # Line endings
//!
//! [`LineEnding::detect`] counts CRLF, lone LF and lone CR terminators and takes
//! the majority; a tie, or a file with no terminator at all, is [`LineEnding::Lf`].
//! A file with mixed endings is therefore saved with its majority ending, which
//! changes the minority lines: only a file whose endings are uniform round-trips
//! byte for byte.
//!
//! # Symlinks
//!
//! [`Document::save`] and [`Document::save_as`] refuse a path that is a symlink
//! (LazyOS never writes through a link), leaving both the link and its target
//! untouched.
//!
//! # Encoding
//!
//! Files must be valid UTF-8. Invalid bytes are rejected with the byte offset of
//! the first bad byte rather than replaced, because a lossy replacement would be
//! written back and corrupt the file.

use std::fs;
use std::path::{Path, PathBuf};

mod atomic;
mod error;

#[cfg(test)]
mod tests;

use atomic::write_atomically;

pub use error::DocumentError;

/// The largest file the editor will load, in bytes (32 MiB). The load is
/// synchronous on the UI thread, so a bigger file is refused rather than
/// freezing the window.
pub const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

/// The line ending a file uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnding {
    /// `\n` (Unix, and the in-memory form).
    Lf,
    /// `\r\n` (Windows).
    CrLf,
}

impl LineEnding {
    /// The ending a text file uses: the majority of its terminators, or
    /// [`LineEnding::Lf`] on a tie or with no terminator.
    pub fn detect(text: &str) -> LineEnding {
        let bytes = text.as_bytes();
        let (mut crlf, mut lf, mut cr) = (0usize, 0usize, 0usize);
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\r' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                crlf += 1;
                i += 2;
                continue;
            }
            match bytes[i] {
                b'\r' => cr += 1,
                b'\n' => lf += 1,
                _ => {}
            }
            i += 1;
        }
        if crlf > lf && crlf >= cr {
            LineEnding::CrLf
        } else {
            LineEnding::Lf
        }
    }

    /// The ending's terminator, `"\n"` or `"\r\n"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
        }
    }
}

/// A file-backed document.
///
/// The struct does not own the text: the editor's buffer does. `Document` owns
/// only the file identity (path, line ending, BOM) and the buffer revision the
/// file was last saved at, which is what [`Document::is_dirty`] compares
/// against.
#[derive(Debug)]
pub struct Document {
    path: Option<PathBuf>,
    line_ending: LineEnding,
    bom: bool,
    saved_revision: u64,
}

impl Document {
    /// A new, untitled document: no path, LF endings, no BOM, saved at revision
    /// zero (a freshly built buffer's revision).
    pub fn untitled() -> Document {
        Document {
            path: None,
            line_ending: LineEnding::Lf,
            bom: false,
            saved_revision: 0,
        }
    }

    /// Loads `path` with the default size cap ([`MAX_FILE_BYTES`]).
    ///
    /// The returned text has `\n` line endings and no BOM, ready for the editor
    /// buffer; the `Document` remembers how to write them back.
    pub fn load(path: impl Into<PathBuf>) -> Result<(Document, String), DocumentError> {
        Document::load_with_limit(path, MAX_FILE_BYTES)
    }

    /// Like [`Document::load`], with an explicit `limit` in bytes, so a test can
    /// exercise the cap without writing a 32 MiB file.
    pub fn load_with_limit(
        path: impl Into<PathBuf>,
        limit: u64,
    ) -> Result<(Document, String), DocumentError> {
        let path = path.into();
        let metadata = fs::metadata(&path).map_err(|source| DocumentError::Io {
            path: path.clone(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(DocumentError::NotAFile { path });
        }
        if metadata.len() > limit {
            return Err(DocumentError::TooLarge {
                path,
                size: metadata.len(),
                limit,
            });
        }
        let bytes = fs::read(&path).map_err(|source| DocumentError::Io {
            path: path.clone(),
            source,
        })?;
        let (bom, body) = match bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
            Some(rest) => (true, rest),
            None => (false, bytes.as_slice()),
        };
        let text = std::str::from_utf8(body).map_err(|error| DocumentError::InvalidUtf8 {
            path: path.clone(),
            offset: error.valid_up_to(),
        })?;
        let document = Document {
            path: Some(path),
            line_ending: LineEnding::detect(text),
            bom,
            saved_revision: 0,
        };
        Ok((document, normalize(text)))
    }

    /// The file's path, or `None` for an untitled document.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The line ending the document is written with.
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// Whether the document is written with a UTF-8 BOM.
    pub fn has_bom(&self) -> bool {
        self.bom
    }

    /// Whether the buffer at `buffer_revision` differs from the last save.
    ///
    /// The revision only ever moves forward, so undoing back to the saved text
    /// still reads as dirty; the editor treats a load as a new, clean buffer
    /// (see [`Editor::set_text`](crate::Editor::set_text)).
    pub fn is_dirty(&self, buffer_revision: u64) -> bool {
        buffer_revision != self.saved_revision
    }

    /// The name shown in the window title: the file name, or `"Untitled"`.
    pub fn display_name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".to_string())
    }

    /// Writes `text` back to the document's path with its stored line ending and
    /// BOM, atomically, and records `revision` as the clean revision.
    ///
    /// Fails with [`DocumentError::NoPath`] for an untitled document.
    pub fn save(&mut self, text: &str, revision: u64) -> Result<(), DocumentError> {
        let path = self.path.clone().ok_or(DocumentError::NoPath)?;
        self.write(&path, text)?;
        self.saved_revision = revision;
        Ok(())
    }

    /// Writes `text` to `path` (refusing a symlink) and makes `path` the
    /// document's path, recording `revision` as clean.
    pub fn save_as(
        &mut self,
        path: impl Into<PathBuf>,
        text: &str,
        revision: u64,
    ) -> Result<(), DocumentError> {
        let path = path.into();
        self.write(&path, text)?;
        self.path = Some(path);
        self.saved_revision = revision;
        Ok(())
    }

    /// Encodes and writes `text` to `path`.
    fn write(&self, path: &Path, text: &str) -> Result<(), DocumentError> {
        let bytes = self.encode(text);
        write_atomically(path, &bytes)
    }

    /// Encodes `text` with the stored line ending and BOM.
    fn encode(&self, text: &str) -> Vec<u8> {
        let normalized = normalize(text);
        let body = match self.line_ending {
            LineEnding::Lf => normalized,
            LineEnding::CrLf => normalized.replace('\n', "\r\n"),
        };
        let mut bytes = Vec::with_capacity(body.len() + 3);
        if self.bom {
            bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        }
        bytes.extend_from_slice(body.as_bytes());
        bytes
    }
}

/// Normalises CRLF and lone CR terminators to `\n`.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(character);
        }
    }
    out
}
