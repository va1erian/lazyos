//! The desktop folder's scan, which runs on a worker thread of its own
//! ([`Scanner`]): directory reads, shortcut reads and the `init` registry call,
//! none of which may hold the thread that draws the taskbar and the desktop.
//! Every filesystem call is timed ([`Timing`]); a slow scan leaves
//! `UI:STALL kind=scan` naming the call that cost the time.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::SystemTime;

use lazyshell::desktop::folder::{self, DirEntry, Item};
use lazyshell::shortcut;
use xui_core::app::Proxy;

use super::desktop::DeskMsg;
use super::services;
use crate::stall;

/// One entry's identity for change detection: name, size, modification time.
pub(super) type Stamp = (String, u64, Option<SystemTime>);

/// The folder's own size and modification time: an entry added, removed or
/// renamed moves the latter, and reading it is one cached `stat`.
pub(super) type DirStamp = (u64, Option<SystemTime>);

/// How long the folder may go without a full listing even when its own stamp
/// did not move (10 s): an edit in place to a shortcut leaves the folder's
/// time alone.
const FULL_SCAN_TICKS: u64 = 1000;

/// What one scan needs: the state of the last one and what to refresh.
pub(super) struct Job {
    pub(super) dir: Option<PathBuf>,
    pub(super) seeded: bool,
    pub(super) refresh_apps: bool,
    pub(super) stamps: Vec<Stamp>,
    pub(super) have_apps: bool,
    /// List the folder whatever its stamp says (a drop just changed it).
    pub(super) force: bool,
    pub(super) dir_stamp: Option<DirStamp>,
    /// The tick of the last full listing.
    pub(super) last_full: u64,
}

/// What one scan found, for the UI thread to fold in ([`Ctx::scan_done`]).
pub struct Outcome {
    pub(super) seeded: bool,
    pub(super) stamps: Option<Vec<Stamp>>,
    pub(super) apps: Option<Vec<services::App>>,
    pub(super) items: Option<Vec<Item>>,
    pub(super) note: Option<(&'static str, String)>,
    pub(super) dir_stamp: Option<DirStamp>,
    pub(super) full_at: Option<u64>,
    /// The registry answered but the folder did not change: show the items
    /// of the last listing again against the new registry.
    pub(super) reapply: bool,
}

impl Outcome {
    /// Nothing changed (a scan that found the folder as it was).
    fn unchanged(seeded: bool) -> Outcome {
        Outcome {
            seeded,
            stamps: None,
            apps: None,
            items: None,
            note: None,
            dir_stamp: None,
            full_at: None,
            reapply: false,
        }
    }
}

/// The scan worker: jobs in, `DeskMsg::Scanned` out through the window's
/// proxy. One thread for the shell's life. A scan is a few directory reads
/// and an `init` call, which on the UI thread froze the taskbar and the
/// desktop for as long as they took (`UI:STALL kind=chore`).
pub struct Scanner {
    pub(super) jobs: mpsc::Sender<Job>,
}

impl Scanner {
    /// Start the worker; `None` when no thread could be made (scans then run
    /// inline, as before).
    pub fn start(proxy: Proxy<DeskMsg>) -> Option<Scanner> {
        let (jobs, queue) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("desk-scan".into())
            .spawn(move || {
                for job in queue {
                    let seeded = job.seeded;
                    // A panicking scan answers "nothing changed": the
                    // desktop keeps its icons and the next scan tries again.
                    let outcome =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| scan(job)))
                            .unwrap_or_else(|_| Outcome::unchanged(seeded));
                    if proxy.send(DeskMsg::Scanned(Box::new(outcome))).is_err() {
                        break;
                    }
                }
            })
            .ok()?;
        Some(Scanner { jobs })
    }
}

/// Where a scan's time went, per kind of filesystem call: sums and worst
/// single call, in microseconds. Printed with the `UI:STALL kind=scan` line
/// of a slow scan, so a slow volume names the call that is slow.
#[derive(Default)]
struct Timing {
    entries: usize,
    shortcuts: usize,
    open_dir_us: u64,
    next_us: u64,
    next_max_us: u64,
    stat_us: u64,
    stat_max_us: u64,
    meta_us: u64,
    meta_max_us: u64,
    open_us: u64,
    open_max_us: u64,
    read_us: u64,
    read_max_us: u64,
    apps_us: u64,
    dir_stat_us: u64,
    refresh: bool,
    /// What a few reference calls cost right after a slow scan (see
    /// [`probe`]); empty when none ran.
    probe: String,
}

impl Timing {
    fn describe(&self) -> String {
        format!(
            "entries={} shortcuts={} refresh={} open_dir={}us next={}us/{}us \
             stat={}us/{}us meta={}us/{}us open={}us/{}us read={}us/{}us apps={}us \
             dir_stat={}us{}",
            self.entries,
            self.shortcuts,
            self.refresh,
            self.open_dir_us,
            self.next_us,
            self.next_max_us,
            self.stat_us,
            self.stat_max_us,
            self.meta_us,
            self.meta_max_us,
            self.open_us,
            self.open_max_us,
            self.read_us,
            self.read_max_us,
            self.apps_us,
            self.dir_stat_us,
            self.probe,
        )
    }
}

/// Microseconds since `since` (a [`stall::start`] stamp).
fn since_us(since: u64) -> u64 {
    stall::start().saturating_sub(since) / 1000
}

/// One scan, timed: a slow one leaves `UI:STALL kind=scan`.
pub(super) fn scan(job: Job) -> Outcome {
    let began = stall::start();
    let mut timing = Timing {
        refresh: job.refresh_apps,
        ..Timing::default()
    };
    let dir = job.dir.clone();
    let outcome = scan_inner(job, &mut timing);
    if since_us(began) >= stall::SLOW_NS / 1000 {
        timing.probe = probe(dir.as_deref());
    }
    stall::finish(began, stall::SLOW_NS, "scan", || timing.describe());
    outcome
}

/// Reference calls run right after a slow scan, to tell what is slow: the
/// shell's descriptors in general, or only the desktop folder. Each is the
/// time of one call in microseconds: opening a file outside the home
/// (`passwd`), opening the home and the desktop folder as directories, a
/// one-millisecond sleep (the timer's real granularity) and one `stat` of the
/// desktop folder. At most one probe per ten seconds.
fn probe(dir: Option<&Path>) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = crate::sys::clock_ticks();
    let last = LAST.load(Ordering::Relaxed);
    if last != 0 && now.saturating_sub(last) < 1000 {
        return String::new();
    }
    LAST.store(now, Ordering::Relaxed);
    let time = |work: &dyn Fn()| {
        let began = stall::start();
        work();
        since_us(began)
    };
    let passwd = time(&|| {
        let _ = fs::File::open(fhs::etc::PASSWD);
    });
    let (home, desk) = match dir {
        Some(dir) => (
            time(&|| {
                if let Some(parent) = dir.parent() {
                    let _ = fs::File::open(parent);
                }
            }),
            time(&|| {
                let _ = fs::File::open(dir);
            }),
        ),
        None => (0, 0),
    };
    let sleep = time(&|| std::thread::sleep(std::time::Duration::from_millis(1)));
    let stat = time(&|| {
        let _ = fs::metadata(dir.unwrap_or(Path::new("/")));
    });
    format!(
        " probe: passwd={passwd}us home={home}us desk={desk}us sleep1ms={sleep}us stat={stat}us"
    )
}

fn scan_inner(job: Job, timing: &mut Timing) -> Outcome {
    let Some(dir) = job.dir.as_deref() else {
        return scan_launchers(job.refresh_apps, timing);
    };
    let mut out = Outcome::unchanged(job.seeded);
    if !job.seeded {
        match seed(dir) {
            Seed::Ready => out.seeded = true,
            Seed::Wait => return out,
            Seed::Failed(why) => {
                out.note = Some(("desk-seed", format!("SHELL:DESKTOP:SEED:FAIL {why}")));
                return out;
            }
        }
    }
    // One cached `stat` says whether the folder changed. A full listing is
    // an `opendir` and a `stat` per entry, and when `/home` is a USB stick
    // opened uncached each directory open is a round of reads from the stick
    // (~0.3 s), which once a second kept the stick busy for ever.
    let began = stall::start();
    let now_stamp = fs::metadata(dir)
        .ok()
        .map(|meta| (meta.len(), meta.modified().ok()));
    timing.dir_stat_us = since_us(began);
    let now = crate::sys::clock_ticks();
    let changed = job.force || now_stamp.is_none() || now_stamp != job.dir_stamp;
    let stale = now.saturating_sub(job.last_full) >= FULL_SCAN_TICKS;
    if !changed && !stale {
        if job.refresh_apps || !job.have_apps {
            out.apps = list_apps(timing);
            out.reapply = out.apps.is_some();
        }
        return out;
    }
    let Ok(stamps) = stamps(dir, timing) else {
        // The folder vanished or cannot be read: try the seed again next
        // time rather than show nothing forever.
        out.seeded = false;
        return out;
    };
    out.dir_stamp = now_stamp;
    out.full_at = Some(now);
    if job.stamps == stamps {
        // The listing is as it was: the shortcuts need no re-reading, only
        // the registry (hidden apps, package icons) may have moved.
        if job.refresh_apps || !job.have_apps {
            out.apps = list_apps(timing);
            out.reapply = out.apps.is_some();
        }
        return out;
    }
    if job.refresh_apps || !job.have_apps {
        out.apps = list_apps(timing);
    }
    let entries = read_entries(dir, &stamps, timing);
    let order = timed_read(&dir.join(folder::ORDER_FILE), timing);
    out.items = Some(folder::items(&entries, order.as_deref()));
    out.stamps = Some(stamps);
    out
}

/// `init`'s app list, timed.
fn list_apps(timing: &mut Timing) -> Option<Vec<services::App>> {
    let began = stall::start();
    let apps = services::list_apps().ok();
    timing.apps_us = since_us(began);
    apps
}

/// No desktop folder: show `sys/ui/desktop` itself.
fn scan_launchers(refresh_apps: bool, timing: &mut Timing) -> Outcome {
    let mut out = Outcome::unchanged(false);
    if !refresh_apps {
        return out;
    }
    let Ok(stored) = services::confd_get(lazyshell::desktop::KEY) else {
        return out;
    };
    let began = stall::start();
    out.apps = services::list_apps().ok();
    timing.apps_us = since_us(began);
    out.items = Some(
        lazyshell::desktop::from_value(stored.as_ref())
            .iter()
            .map(Item::launcher)
            .collect(),
    );
    out
}

/// What [`seed`] found.
enum Seed {
    /// The folder exists (or was just written).
    Ready,
    /// `confd` is not up yet: try again.
    Wait,
    Failed(String),
}

/// Create and fill a missing folder from `sys/ui/desktop`. Waits for `confd`
/// so a configured list is not replaced by the defaults just because `confd`
/// was late.
fn seed(dir: &Path) -> Seed {
    if dir.is_dir() {
        return Seed::Ready;
    }
    let Ok(stored) = services::confd_get(lazyshell::desktop::KEY) else {
        return Seed::Wait;
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
            Seed::Ready
        }
        Err(error) => Seed::Failed(error.to_string()),
    }
}

/// Write the seed into a new folder `dir`. `create_dir` (not `_all`) and
/// `create_new` files: a folder something else made meanwhile is left as is.
/// If a write fails, the files this call created are removed and then the
/// folder if nothing else was put in it meanwhile, so the next poll seeds
/// again instead of counting a half-written folder as done.
fn write_seed(dir: &Path, files: &[(String, String)]) -> io::Result<()> {
    fs::create_dir(dir)?;
    let mut created = Vec::new();
    let written = files.iter().try_for_each(|(name, text)| {
        let path = dir.join(name);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        created.push(path);
        file.write_all(text.as_bytes())
    });
    if written.is_err() {
        for path in &created {
            let _ = fs::remove_file(path);
        }
        // Fails, and keeps the folder, when someone else's file is in it.
        let _ = fs::remove_dir(dir);
    }
    written
}

/// The folder's visible entries and the order file, stamped, sorted by name.
fn stamps(dir: &Path, timing: &mut Timing) -> io::Result<Vec<Stamp>> {
    let began = stall::start();
    let listing = fs::read_dir(dir);
    timing.open_dir_us = since_us(began);
    let mut listing = listing?;
    let mut stamps = Vec::new();
    loop {
        let began = stall::start();
        let next = listing.next();
        let took = since_us(began);
        timing.next_us += took;
        timing.next_max_us = timing.next_max_us.max(took);
        let Some(entry) = next else {
            break;
        };
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') && name != folder::ORDER_FILE {
            continue;
        }
        let began = stall::start();
        let meta = entry.metadata().ok();
        let took = since_us(began);
        timing.stat_us += took;
        timing.stat_max_us = timing.stat_max_us.max(took);
        let size = meta.as_ref().map_or(0, fs::Metadata::len);
        let modified = meta.and_then(|meta| meta.modified().ok());
        stamps.push((name, size, modified));
    }
    timing.entries = stamps.len();
    stamps.sort();
    Ok(stamps)
}

/// The entries the stamps list, with each shortcut's text read.
fn read_entries(dir: &Path, stamps: &[Stamp], timing: &mut Timing) -> Vec<DirEntry> {
    stamps
        .iter()
        .filter(|(name, _, _)| name != folder::ORDER_FILE)
        .map(|(name, _, _)| {
            let path = dir.join(name);
            let began = stall::start();
            let is_dir = fs::metadata(&path).is_ok_and(|meta| meta.is_dir());
            let took = since_us(began);
            timing.meta_us += took;
            timing.meta_max_us = timing.meta_max_us.max(took);
            let text = (!is_dir && shortcut::is_shortcut_name(name))
                .then(|| {
                    timing.shortcuts += 1;
                    timed_read(&path, timing)
                })
                .flatten();
            DirEntry {
                name: name.clone(),
                is_dir,
                text,
            }
        })
        .collect()
}

/// A small text file's contents, with the open and the read timed apart:
/// `None` when missing, unreadable, not UTF-8 or longer than a shortcut may
/// be.
fn timed_read(path: &Path, timing: &mut Timing) -> Option<String> {
    let began = stall::start();
    let file = fs::File::open(path);
    let took = since_us(began);
    timing.open_us += took;
    timing.open_max_us = timing.open_max_us.max(took);
    let file = file.ok()?;
    let began = stall::start();
    let mut text = String::new();
    let read = file
        .take(shortcut::MAX_BYTES + 1)
        .read_to_string(&mut text)
        .ok();
    let took = since_us(began);
    timing.read_us += took;
    timing.read_max_us = timing.read_max_us.max(took);
    (read? as u64 <= shortcut::MAX_BYTES).then_some(text)
}
