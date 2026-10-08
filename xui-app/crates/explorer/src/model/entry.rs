#![forbid(unsafe_code)]

//! The sorted listing of one folder, and the [`IconModel`] and [`ListModel`]
//! views over it (the icon view's tiles and the details view's rows).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

use xui_core::backend::Canvas;
use xui_core::geometry::Rect;
use xui_core::icon::IconRef;
use xui_core::theme::Theme;
use xui_core::widget::{IconModel, ListModel};

use super::sort::{SortOrder, sort_entries};
use super::village;
use super::{format_size, format_time};
use crate::platform::{Kind, Platform, RawEntry};

/// One listed item, with its display strings resolved once at load time so the
/// paint path never allocates.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The item's raw name, used for selection remapping and deletion.
    pub name: OsString,
    /// The name as shown, with a non-UTF-8 name lossily converted.
    pub display: String,
    /// The second tile line: the size for a file, `"Folder"` for a directory.
    pub detail: String,
    /// What the item is.
    pub kind: Kind,
    /// The file's size in bytes, when it has one.
    pub size: Option<u64>,
    /// The last modification time, when the platform reports one.
    pub modified: Option<SystemTime>,
    /// The details view's Type column: `"Folder"`, `"PNG File"`, `"File"`.
    pub type_name: String,
    /// The details view's Size column (empty for a folder).
    pub size_text: String,
    /// The details view's Modified column (empty when unknown).
    pub modified_text: String,
}

impl Entry {
    /// Resolves a raw entry into its display form.
    pub fn from_raw(raw: RawEntry) -> Entry {
        let display = raw.name.to_string_lossy().into_owned();
        let size_text = match raw.meta.kind {
            Kind::Dir => String::new(),
            Kind::File | Kind::Symlink => raw.meta.size.map(format_size).unwrap_or_default(),
        };
        let detail = match raw.meta.kind {
            Kind::Dir => "Folder".to_string(),
            Kind::File | Kind::Symlink if size_text.is_empty() => "File".to_string(),
            Kind::File | Kind::Symlink => size_text.clone(),
        };
        Entry {
            type_name: type_name(raw.meta.kind, &raw.name),
            modified_text: raw.meta.modified.map(format_time).unwrap_or_default(),
            modified: raw.meta.modified,
            name: raw.name,
            display,
            detail,
            kind: raw.meta.kind,
            size: raw.meta.size,
            size_text,
        }
    }
}

/// The Type column's text: a folder, a link, or a file named by its
/// extension in capitals (`photo.png` is a `"PNG File"`).
pub fn type_name(kind: Kind, name: &OsStr) -> String {
    match kind {
        Kind::Dir => "Folder".to_string(),
        Kind::Symlink => "Link".to_string(),
        Kind::File => match Path::new(name).extension().and_then(OsStr::to_str) {
            Some(extension) if !extension.is_empty() => {
                format!("{} File", extension.to_uppercase())
            }
            _ => "File".to_string(),
        },
    }
}

/// A folder's contents: its entries (folders first, then files, in the
/// window's sort order) and, when the folder could not be read, an error
/// message with an empty entry list.
#[derive(Clone, Debug)]
pub struct Listing {
    /// The folder this listing is of.
    pub dir: PathBuf,
    /// The entries, folders first.
    pub entries: Vec<Entry>,
    /// The read error, when the folder could not be listed.
    pub error: Option<String>,
}

impl Listing {
    /// Lists `dir` through `platform`, sorted by `order`. A read failure
    /// yields an empty listing carrying the error text rather than an empty
    /// view with no explanation.
    pub fn load(platform: &dyn Platform, dir: &Path, order: SortOrder) -> Listing {
        match platform.list(dir) {
            Ok(raw) => {
                let mut entries: Vec<Entry> = raw.into_iter().map(Entry::from_raw).collect();
                sort_entries(&mut entries, order);
                Listing {
                    dir: dir.to_path_buf(),
                    entries,
                    error: None,
                }
            }
            Err(error) => Listing {
                dir: dir.to_path_buf(),
                entries: Vec::new(),
                error: Some(error.to_string()),
            },
        }
    }

    /// The same entries in another order.
    pub fn sorted(&self, order: SortOrder) -> Listing {
        let mut listing = self.clone();
        sort_entries(&mut listing.entries, order);
        listing
    }

    /// The index of the entry named `name`, if present.
    pub fn index_of(&self, name: &OsStr) -> Option<usize> {
        self.entries.iter().position(|entry| entry.name == name)
    }

    /// The names of the selected entries, in listing order. Used to carry a
    /// selection across a refresh by identity rather than by index.
    pub fn names_of(&self, selection: &[usize]) -> Vec<OsString> {
        selection
            .iter()
            .filter_map(|index| self.entries.get(*index).map(|entry| entry.name.clone()))
            .collect()
    }

    /// The indices of `names` that still exist, ascending and deduplicated.
    pub fn indices_of(&self, names: &[OsString]) -> Vec<usize> {
        let mut indices: Vec<usize> = names
            .iter()
            .filter_map(|name| self.index_of(name))
            .collect();
        indices.sort_unstable();
        indices.dedup();
        indices
    }

    /// The selected entries, in listing order.
    pub fn selected(&self, selection: &[usize]) -> Vec<&Entry> {
        selection
            .iter()
            .filter_map(|index| self.entries.get(*index))
            .collect()
    }
}

/// A shared [`Listing`] as both an [`IconModel`] and a [`ListModel`], so the
/// window and its two views read the same entries without copying them.
#[derive(Clone)]
pub struct SharedListing {
    listing: Rc<Listing>,
}

impl SharedListing {
    /// Wraps `listing`.
    pub fn new(listing: Rc<Listing>) -> SharedListing {
        SharedListing { listing }
    }
}

impl IconModel for SharedListing {
    fn items(&self) -> usize {
        self.listing.entries.len()
    }

    fn icon(&self, item: usize) -> Option<IconRef> {
        Some(village::icon_ref(self.listing.entries.get(item)?))
    }

    fn paint_icon(
        &self,
        item: usize,
        canvas: &mut dyn Canvas,
        rect: Rect,
        theme: &Theme,
        dpi: u32,
    ) -> bool {
        let Some(entry) = self.listing.entries.get(item) else {
            return false;
        };
        village::paint(entry, canvas, rect, theme, dpi)
    }

    fn line(&self, item: usize, line: usize) -> Option<&str> {
        let entry = self.listing.entries.get(item)?;
        match line {
            0 => Some(&entry.display),
            1 => Some(&entry.detail),
            _ => None,
        }
    }
}

/// The details view's columns, in [`SortKey::ALL`](super::SortKey::ALL)
/// order: Name, Size, Type, Modified.
impl ListModel for SharedListing {
    fn rows(&self) -> usize {
        self.listing.entries.len()
    }

    fn cell(&self, row: usize, column: usize) -> Option<&str> {
        let entry = self.listing.entries.get(row)?;
        match column {
            0 => Some(&entry.display),
            1 => Some(&entry.size_text),
            2 => Some(&entry.type_name),
            3 => Some(&entry.modified_text),
            _ => None,
        }
    }

    fn icon(&self, row: usize) -> Option<IconRef> {
        Some(village::icon_ref(self.listing.entries.get(row)?))
    }
}
