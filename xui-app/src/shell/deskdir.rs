//! The desktop folder on disk: `$HOME/Desktop` (`fhs::state::DESKTOP_DIR`),
//! whose entries are the desktop's icons (`lazyshell::desktop::folder`).
//!
//! The first time the shell finds the folder missing it creates it and
//! writes one shortcut per `sys/ui/desktop` launcher, plus the order file
//! that keeps them in place; from then on the folder is the user's. The
//! heartbeat polls it about once a second: a listing whose names, sizes and
//! times did not change costs one directory read, and only a change re-reads
//! the shortcuts and asks `init` for the icons. Without a usable `$HOME` the
//! desktop shows the launchers directly, as it did before the folder.
//!
//! Every file here is the user's (or another app's), so it is read with a
//! size cap and parsed strictly; a drop into the folder goes through the
//! explorer's careful copy/move (`xui_explorer::std_platform::drop_into`).
//!
//! Serial markers: `SHELL:DESKTOP:SEEDED dir=<dir> n=<shortcuts>`,
//! `SHELL:DESKTOP:SEED:FAIL <why>`, `SHELL:DESKTOP:ICONS n=<n>`,
//! `SHELL:DESKTOP:DROP:<copied>:<moved>:<failed>`.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use lazyshell::desktop::folder::{self, DirEntry, Item, Kind};
use lazyshell::menu::{listed_hidden, Listed};
use lazyshell::shortcut;
use xui_explorer::std_platform::{drop_into, Intent};

use super::ctx::Ctx;
use super::services;

/// One entry's identity for change detection: name, size, modification time.
type Stamp = (String, u64, Option<SystemTime>);

/// What the shell last saw of the folder.
#[derive(Default)]
pub struct Watch {
    /// The folder, once `$HOME` gave one.
    pub dir: Option<PathBuf>,
    /// Whether the seed step is done (seeded, or the folder was there).
    seeded: bool,
    /// The listing the icons were built from.
    stamps: Vec<Stamp>,
    /// `init`'s registry the icons were last resolved against.
    apps: Vec<services::App>,
}

impl Watch {
    /// The watch for this process's `$HOME` (checked: absolute, a folder).
    pub fn new() -> Watch {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let dir = home
            .filter(|home| home.is_absolute() && home.is_dir())
            .map(|home| home.join(fhs::state::DESKTOP_DIR));
        Watch {
            dir,
            ..Watch::default()
        }
    }
}

impl Ctx {
    /// Bring the desktop icons up to date; `refresh_apps` also asks `init`
    /// again (for hidden apps and package icons). Returns whether the icons
    /// changed (and sets `icons_changed`).
    pub fn reload_desktop(&self, refresh_apps: bool) -> bool {
        let dir = self.desk.borrow().dir.clone();
        let Some(dir) = dir else {
            return self.show_launchers(refresh_apps);
        };
        if !self.desk.borrow().seeded && !self.seed(&dir) {
            return false;
        }
        let Ok(stamps) = stamps(&dir) else {
            // The folder vanished or cannot be read: try the seed again next
            // time rather than show nothing forever.
            self.desk.borrow_mut().seeded = false;
            return false;
        };
        let moved = self.desk.borrow().stamps != stamps;
        if !moved && !refresh_apps {
            return false;
        }
        if refresh_apps || self.desk.borrow().apps.is_empty() {
            if let Ok(apps) = services::list_apps() {
                self.desk.borrow_mut().apps = apps;
            }
        }
        let entries = read_entries(&dir, &stamps);
        let order = read_capped(&dir.join(folder::ORDER_FILE));
        let items = folder::items(&entries, order.as_deref());
        self.desk.borrow_mut().stamps = stamps;
        self.set_icons(items)
    }

    /// No desktop folder: show `sys/ui/desktop` itself.
    fn show_launchers(&self, refresh_apps: bool) -> bool {
        if !refresh_apps {
            return false;
        }
        let Ok(stored) = services::confd_get(lazyshell::desktop::KEY) else {
            return false;
        };
        if let Ok(apps) = services::list_apps() {
            self.desk.borrow_mut().apps = apps;
        }
        let items = lazyshell::desktop::from_value(stored.as_ref())
            .iter()
            .map(Item::launcher)
            .collect();
        self.set_icons(items)
    }

    /// Create and fill a missing folder from `sys/ui/desktop`; `true` once
    /// the folder exists. Waits for `confd` so a configured list is not
    /// replaced by the defaults just because `confd` was late.
    fn seed(&self, dir: &Path) -> bool {
        if dir.is_dir() {
            self.desk.borrow_mut().seeded = true;
            return true;
        }
        let Ok(stored) = services::confd_get(lazyshell::desktop::KEY) else {
            return false;
        };
        let launchers = lazyshell::desktop::from_value(stored.as_ref());
        let files = folder::seed(&launchers);
        match write_seed(dir, &files) {
            Ok(()) => {
                println!(
                    "SHELL:DESKTOP:SEEDED dir={} n={}",
                    dir.display(),
                    files.len() - 1
                );
                self.desk.borrow_mut().seeded = true;
                true
            }
            Err(error) => {
                self.note("desk-seed", || format!("SHELL:DESKTOP:SEED:FAIL {error}"));
                false
            }
        }
    }

    /// Replace the icons with `items`, minus the apps this user hides, with
    /// each app shortcut's package icon; `true` when anything changed.
    fn set_icons(&self, items: Vec<Item>) -> bool {
        let desk = self.desk.borrow();
        let listed: Vec<Listed<'_>> = desk.apps.iter().map(services::App::listed).collect();
        let items: Vec<Item> = items
            .into_iter()
            .filter(|item| item.app().is_none_or(|app| !listed_hidden(&listed, app)))
            .collect();
        let images: Vec<String> = items
            .iter()
            .map(|item| {
                item.app()
                    .and_then(|app| {
                        desk.apps
                            .iter()
                            .find(|row| deskmenu::same_app(&row.id, app))
                    })
                    .map(|row| row.icon.clone())
                    .unwrap_or_default()
            })
            .collect();
        drop(desk);
        if *self.icons.borrow() == items && *self.icon_images.borrow() == images {
            return false;
        }
        println!("SHELL:DESKTOP:ICONS n={}", items.len());
        *self.icons.borrow_mut() = items;
        *self.icon_images.borrow_mut() = images;
        self.icons_changed.set(true);
        true
    }

    /// The path icon `item` stands for in the desktop folder, if it has one.
    pub fn icon_path(&self, item: &Item) -> Option<PathBuf> {
        let dir = self.desk.borrow().dir.clone()?;
        (!item.name.is_empty()).then(|| dir.join(&item.name))
    }

    /// A `text/uri-list` dropped on the desktop: copy (or with Shift, move)
    /// the paths into the folder, then re-read it.
    pub fn drop_on_desktop(&self, paths: &[PathBuf]) {
        let Some(dir) = self.desk.borrow().dir.clone() else {
            return;
        };
        let held = self.backend.held_modifiers();
        let intent = Intent {
            ours: false,
            ctrl: held.ctrl,
            shift: held.shift,
        };
        let report = drop_into(paths, &dir, intent);
        for (path, error) in &report.failed {
            println!("SHELL:DESKTOP:DROP:FAIL {}: {error}", path.display());
        }
        println!(
            "SHELL:DESKTOP:DROP:{}:{}:{}",
            report.copied,
            report.moved,
            report.failed.len()
        );
        self.reload_desktop(false);
    }
}

/// Write the seed into a new folder `dir`. `create_dir` (not `_all`) and
/// `create_new` files: a folder something else made meanwhile is left as is.
fn write_seed(dir: &Path, files: &[(String, String)]) -> io::Result<()> {
    fs::create_dir(dir)?;
    for (name, text) in files {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(name))?;
        file.write_all(text.as_bytes())?;
    }
    Ok(())
}

/// The folder's visible entries and the order file, stamped, sorted by name.
fn stamps(dir: &Path) -> io::Result<Vec<Stamp>> {
    let mut stamps = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') && name != folder::ORDER_FILE {
            continue;
        }
        let meta = entry.metadata().ok();
        let size = meta.as_ref().map_or(0, fs::Metadata::len);
        let modified = meta.and_then(|meta| meta.modified().ok());
        stamps.push((name, size, modified));
    }
    stamps.sort();
    Ok(stamps)
}

/// The entries the stamps list, with each shortcut's text read.
fn read_entries(dir: &Path, stamps: &[Stamp]) -> Vec<DirEntry> {
    stamps
        .iter()
        .filter(|(name, _, _)| name != folder::ORDER_FILE)
        .map(|(name, _, _)| {
            let path = dir.join(name);
            let is_dir = fs::metadata(&path).is_ok_and(|meta| meta.is_dir());
            let text = (!is_dir && shortcut::is_shortcut_name(name))
                .then(|| read_capped(&path))
                .flatten();
            DirEntry {
                name: name.clone(),
                is_dir,
                text,
            }
        })
        .collect()
}

/// A small text file's contents: `None` when missing, unreadable, not UTF-8
/// or longer than a shortcut may be.
fn read_capped(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut text = String::new();
    let read = file
        .take(shortcut::MAX_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    (read as u64 <= shortcut::MAX_BYTES).then_some(text)
}

/// What opening `item` does: launch its app, browse its folder in Files, or
/// hand its file to `mimed`.
pub enum Open {
    Launch(String),
    Browse(PathBuf),
    File(PathBuf),
}

/// How to open `item`, found at `path` (its place in the folder, if any).
pub fn open_action(item: &Item, path: Option<PathBuf>) -> Option<Open> {
    match &item.kind {
        Kind::App(app) => Some(Open::Launch(app.clone())),
        Kind::Link(target) => {
            let target = PathBuf::from(target);
            Some(if target.is_dir() {
                Open::Browse(target)
            } else {
                Open::File(target)
            })
        }
        Kind::Folder => path.map(Open::Browse),
        Kind::File => path.map(Open::File),
    }
}
