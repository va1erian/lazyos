#![forbid(unsafe_code)]

//! LazyWriter's file I/O, ported from the wordpad example's `files.rs`: the
//! widget library does none, so it lives here.
//!
//! Every input is untrusted. Reads are capped ([`MAX_DOCUMENT_BYTES`],
//! [`MAX_IMAGE_BYTES`]), a picture's header is checked before it is decoded,
//! and every failure comes back as an [`OpenError`] or a message, never a
//! panic. Writes go through the [`Host`]'s writer (atomic on LazyOS).

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use xui_core::Dip;
use xui_core::image::Image;
use xui_rich_text::format::{FormatError, ImageExport, from_json, to_json, to_markdown};
use xui_rich_text::model::{Document, InlineImage, Wrap};

use crate::host::Host;
use crate::names::{
    self, MAX_DOCUMENT_BYTES, MAX_IMAGE_BYTES, ReadError, image_entry, images_folder,
};
use crate::probe;

/// The widest an inserted picture is shown, in design units.
pub const MAX_IMAGE_WIDTH: f32 = 360.0;

/// Why a document could not be opened.
#[derive(Debug)]
pub enum OpenError {
    /// The file could not be read, or is over the cap.
    Read(ReadError),
    /// A `.lzw` file that is not UTF-8 text.
    NotText,
    /// The JSON is malformed, of another version, or breaks an invariant.
    Format(FormatError),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Read(error) => write!(f, "{error}"),
            OpenError::NotText => write!(f, "not a LazyWriter document (not text)"),
            OpenError::Format(error) => write!(f, "{error}"),
        }
    }
}

/// Parses `bytes` as a document: plain text when `plain`, else the native JSON.
pub fn parse_document(bytes: &[u8], plain: bool) -> Result<Document, OpenError> {
    if plain {
        return Ok(Document::from_plain_text(&String::from_utf8_lossy(bytes)));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| OpenError::NotText)?;
    from_json(text).map_err(OpenError::Format)
}

/// Opens `path`: a `.txt` as plain text, anything else as a `.lzw`.
pub fn open_document(path: &Path) -> Result<Document, OpenError> {
    let bytes = names::read_file_capped(path, MAX_DOCUMENT_BYTES).map_err(OpenError::Read)?;
    parse_document(&bytes, names::is_plain_text(path))
}

/// Saves `doc` to `path` in the native format.
pub fn save_document(host: &Host, path: &Path, doc: &Document) -> Result<(), String> {
    (host.write)(path, to_json(doc).as_bytes())
}

/// Writes `doc` as Markdown to `path`. Its pictures are saved as PNGs in
/// `<stem>_images/` beside it (created only when there is one) and linked
/// relatively.
pub fn export_markdown(host: &Host, path: &Path, doc: &Document) -> Result<(), String> {
    let dir = images_folder(path);
    let count = Rc::new(Cell::new(0usize));
    let failure: Rc<RefCell<Option<String>>> = Rc::default();

    let (counter, error, writer, md) = (
        Rc::clone(&count),
        Rc::clone(&failure),
        Rc::clone(&host.write),
        path.to_path_buf(),
    );
    let export = ImageExport::Callback(Box::new(move |image, _alt| {
        counter.set(counter.get() + 1);
        let (name, link) = image_entry(&md, counter.get());
        let written = std::fs::create_dir_all(&dir)
            .map_err(|e| e.to_string())
            .and_then(|()| image.encode_png().map_err(|e| e.to_string()))
            .and_then(|png| writer(&dir.join(&name), &png));
        if let Err(e) = written {
            error.borrow_mut().get_or_insert(e);
        }
        link
    }));
    let markdown = to_markdown(doc, &export);
    if let Some(error) = failure.borrow_mut().take() {
        return Err(error);
    }
    (host.write)(path, markdown.as_bytes())
}

/// Decodes a picture file into an inline image no wider than
/// [`MAX_IMAGE_WIDTH`], keeping its aspect ratio.
pub fn load_image(path: &Path) -> Result<InlineImage, String> {
    let bytes = names::read_file_capped(path, MAX_IMAGE_BYTES).map_err(|e| e.to_string())?;
    let alt = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    image_from_bytes(&bytes, alt)
}

/// [`load_image`] over bytes already read.
pub fn image_from_bytes(bytes: &[u8], alt: String) -> Result<InlineImage, String> {
    let size = probe::dimensions(bytes).ok_or("not a PNG or JPEG picture")?;
    if !probe::fits(size) {
        return Err(format!(
            "the picture is {} x {} pixels; the largest is 4096 x 4096",
            size.0, size.1
        ));
    }
    let image = Image::decode(bytes).map_err(|e| e.to_string())?;
    let (w, h) = (image.width() as f32, image.height() as f32);
    let scale = (MAX_IMAGE_WIDTH / w).min(1.0);
    Ok(InlineImage {
        image: Arc::new(image),
        size: (Dip(w * scale), Dip(h * scale)),
        wrap: Wrap::Inline,
        alt,
    })
}

/// The number of words in `doc`, for the status bar.
pub fn word_count(doc: &Document) -> usize {
    doc.to_plain_text().split_whitespace().count()
}

#[cfg(test)]
mod tests;
