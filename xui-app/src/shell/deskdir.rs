//! The desktop folder on disk: `$HOME/Desktop` (`fhs::state::DESKTOP_DIR`),
//! whose entries are the desktop's icons (`lazyshell::desktop::folder`).
//!
//! The first time the shell finds the folder missing it creates it and
//! writes one shortcut per `sys/ui/desktop` launcher, plus the order file
//! that keeps them in place; from then on the folder is the user's. The
//! heartbeat polls it about once a second: a listing whose names, sizes and
//! times did not change costs one directory read, and only a change re-reads
//! the shortcuts and asks `init` for the icons. The reads run on the scan
//! worker (`deskscan.rs`), never on the thread that draws the desktop. Without a usable `$HOME` the
//! desktop shows the launchers directly, as it did before the folder.
//!
//! Every file here is the user's (or another app's), so it is read with a
//! size cap and parsed strictly; a drop into the folder goes through the
//! explorer's careful copy/move (`xui_explorer::std_platform::drop_into`).
//!
//! Serial markers: `SHELL:DESKTOP:SEEDED dir=<dir> n=<shortcuts>`,
//! `SHELL:DESKTOP:SEED:FAIL <why>`, `SHELL:DESKTOP:ICONS n=<n>`,
//! `SHELL:DESKTOP:DROP:<copied>:<moved>:<failed>`.

use std::path::PathBuf;

use lazyshell::desktop::folder::{Item, Kind};
use lazyshell::menu::{listed_hidden, Listed};
use xui_core::app::Proxy;
use xui_explorer::std_platform::{drop_into, Intent};

use super::ctx::Ctx;
use super::deskscan::{scan, DirStamp, Job, Outcome, Scanner, Stamp};
use super::desktop::DeskMsg;
use super::services;

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
    /// The worker that scans the folder off the UI thread, once the desktop
    /// window exists (before it, [`Ctx::reload_desktop`] scans inline).
    scanner: Option<Scanner>,
    /// A scan is out; its answer arrives as [`DeskMsg::Scanned`].
    busy: bool,
    /// A registry re-read was asked for while a scan ran: run it when the
    /// answer is in.
    again: Option<bool>,
    /// A scan has answered at least once (the icons are the folder's, not
    /// the empty first view).
    loaded: bool,
    /// The folder's own stamp at the last full listing, and its tick: a
    /// poll that finds the stamp unchanged does not list the folder again.
    dir_stamp: Option<DirStamp>,
    last_full: u64,
    /// The items of the last listing, before the hidden apps are removed,
    /// so a registry change can be applied without reading the folder.
    items: Vec<Item>,
    /// A forced listing was asked for while a scan ran.
    again_force: bool,
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
    /// again (for hidden apps and package icons). With the scan worker
    /// running this only hands it the job (the answer is folded in by
    /// [`Ctx::scan_done`], and a request made while one runs is queued);
    /// without it the scan runs here. Returns whether the icons changed
    /// (and sets `icons_changed`), which only an inline scan can know.
    pub fn reload_desktop(&self, refresh_apps: bool) -> bool {
        self.submit(refresh_apps, false)
    }

    /// [`Ctx::reload_desktop`] listing the folder whatever its stamp says:
    /// right after the shell itself changed it (a drop).
    pub fn reload_desktop_now(&self) -> bool {
        self.submit(false, true)
    }

    fn submit(&self, refresh_apps: bool, force: bool) -> bool {
        let job = {
            let desk = self.desk.borrow();
            Job {
                dir: desk.dir.clone(),
                seeded: desk.seeded,
                refresh_apps,
                stamps: desk.stamps.clone(),
                have_apps: !desk.apps.is_empty(),
                force,
                dir_stamp: desk.dir_stamp,
                last_full: desk.last_full,
            }
        };
        {
            let mut desk = self.desk.borrow_mut();
            if desk.busy {
                // A plain folder look is skipped (the next heartbeat asks
                // again); a registry refresh or a forced listing is not lost.
                if refresh_apps {
                    desk.again = Some(true);
                }
                if force {
                    desk.again_force = true;
                }
                return false;
            }
            if let Some(scanner) = &desk.scanner {
                if scanner.jobs.send(job).is_ok() {
                    desk.busy = true;
                } else {
                    // The worker is gone: scan here from the next call on.
                    desk.scanner = None;
                }
                return false;
            }
        }
        self.apply_scan(scan(job))
    }

    /// The worker's answer: fold it in, then run a reload that was asked for
    /// meanwhile.
    pub fn scan_done(&self, outcome: Outcome) {
        self.apply_scan(outcome);
        let (again, force) = {
            let mut desk = self.desk.borrow_mut();
            desk.busy = false;
            (desk.again.take(), std::mem::take(&mut desk.again_force))
        };
        if again.is_some() || force {
            self.submit(again.unwrap_or(false), force);
        }
    }

    /// Put a scan's findings in the watch and the icons.
    fn apply_scan(&self, outcome: Outcome) -> bool {
        {
            let mut desk = self.desk.borrow_mut();
            desk.loaded = true;
            desk.seeded = outcome.seeded;
            if let Some(stamps) = outcome.stamps {
                desk.stamps = stamps;
            }
            if let Some(apps) = outcome.apps {
                desk.apps = apps;
            }
            if let Some(stamp) = outcome.dir_stamp {
                desk.dir_stamp = Some(stamp);
            }
            if let Some(at) = outcome.full_at {
                desk.last_full = at;
            }
        }
        if let Some((key, line)) = outcome.note {
            self.note(key, || line);
        }
        let items = match outcome.items {
            Some(items) => {
                self.desk.borrow_mut().items = items.clone();
                Some(items)
            }
            None if outcome.reapply => Some(self.desk.borrow().items.clone()),
            None => None,
        };
        match items {
            Some(items) => self.set_icons(items),
            None => false,
        }
    }

    /// Start the scan worker on the desktop window's proxy.
    pub fn start_scanner(&self, proxy: Proxy<DeskMsg>) {
        let mut desk = self.desk.borrow_mut();
        desk.scanner = Scanner::start(proxy);
        // A scan the previous worker never answered (the window was rebuilt).
        desk.busy = false;
        desk.again = None;
    }

    /// Whether a scan has answered, so the icon count is the folder's.
    pub fn desktop_loaded(&self) -> bool {
        self.desk.borrow().loaded
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

    /// A `text/uri-list` dropped on the desktop with `held` modifiers: copy
    /// (or with Shift, move) the paths into the folder, then re-read it.
    pub fn drop_on_desktop(&self, paths: &[PathBuf], held: xui_core::Modifiers) {
        let Some(dir) = self.desk.borrow().dir.clone() else {
            return;
        };
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
        self.reload_desktop_now();
    }
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
