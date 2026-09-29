#![forbid(unsafe_code)]

//! The sorted listing of one folder and the [`IconModel`] view over it.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_core::backend::Canvas;
use xui_core::geometry::Rect;
use xui_core::icon::IconRef;
use xui_core::theme::Theme;
use xui_core::widget::IconModel;

use super::flash::Flash;
use super::format_size;
use super::sort::sort_entries;
use super::village;
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
}

impl Entry {
    /// Resolves a raw entry into its display form.
    pub fn from_raw(raw: RawEntry) -> Entry {
        let display = raw.name.to_string_lossy().into_owned();
        let detail = match raw.meta.kind {
            Kind::Dir => "Folder".to_string(),
            Kind::File | Kind::Symlink => raw
                .meta
                .size
                .map(format_size)
                .unwrap_or_else(|| "File".to_string()),
        };
        Entry {
            name: raw.name,
            display,
            detail,
            kind: raw.meta.kind,
            size: raw.meta.size,
        }
    }
}

/// A folder's contents: its entries (folders first, then files, each group
/// sorted case-insensitively) and, when the folder could not be read, an error
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
    /// Lists `dir` through `platform`. A read failure yields an empty listing
    /// carrying the error text rather than an empty view with no explanation.
    pub fn load(platform: &dyn Platform, dir: &Path) -> Listing {
        match platform.list(dir) {
            Ok(raw) => {
                let mut entries: Vec<Entry> = raw.into_iter().map(Entry::from_raw).collect();
                sort_entries(&mut entries);
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

    /// An empty listing for `dir`, used before the first load.
    pub fn empty(dir: &Path) -> Listing {
        Listing {
            dir: dir.to_path_buf(),
            entries: Vec::new(),
            error: None,
        }
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

/// A shared [`Listing`] as an [`IconModel`], so the app and the view read the
/// same entries without copying them.
///
/// It also reads the window's [`Flash`] (by borrowed name, so no per-tile
/// allocation), so a just-opened folder draws its open icon for two seconds.
#[derive(Clone)]
pub struct SharedListing {
    listing: Rc<Listing>,
    flash: Rc<Flash>,
}

impl SharedListing {
    /// Wraps `listing` with a private flash, so nothing flashes.
    pub fn new(listing: Rc<Listing>) -> SharedListing {
        SharedListing::with_flash(listing, Rc::new(Flash::new()))
    }

    /// Wraps `listing`, reading `flash` for the open-folder state.
    pub fn with_flash(listing: Rc<Listing>, flash: Rc<Flash>) -> SharedListing {
        SharedListing { listing, flash }
    }
}

impl IconModel for SharedListing {
    fn items(&self) -> usize {
        self.listing.entries.len()
    }

    fn icon(&self, item: usize) -> Option<IconRef> {
        let entry = self.listing.entries.get(item)?;
        Some(village::icon_ref(
            entry,
            self.flash.is_flashing(&entry.name),
        ))
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
        village::paint(
            entry,
            self.flash.is_flashing(&entry.name),
            canvas,
            rect,
            theme,
            dpi,
        )
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
