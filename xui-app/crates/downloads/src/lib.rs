//! Downloads: what the engine will not show goes to the user's Downloads folder.
//!
//! [`Saver`] chooses where each download goes. It names the file from the server's suggestion
//! (sanitised, never trusted as a path), picks a name that is not taken (`file (1).zip`), and
//! hands the engine `<name>.part` to write; the window renames it once every byte arrived
//! ([`Saver::finish`]) and removes it when the download fails or is cancelled. Nothing is ever
//! written outside the one folder: the engine only learns a path this module built from
//! [`sanitize`]'s output, and the folder is created here, not by the page.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

/// The person's downloads folder below `$HOME` (LazyWeb+'s and webd's).
pub const FOLDER: &str = "Downloads";

/// The longest file name kept, in bytes.
const MAX_NAME: usize = 120;

/// The most downloads under way at once (reserved and not yet [`Saver::finish`]ed): a page that starts
/// them in a loop (no gesture is needed for an attachment) must not fill the disk or the bookkeeping.
/// Finished ones free their place, so a long session is never locked out for good.
pub const MAX_DOWNLOADS: usize = 256;

/// The most downloads under way at once for one owner ([`Saver::reserve_for`]): webd shares one saver
/// between its clients, and one client's page must not use up the others' places.
pub const MAX_DOWNLOADS_PER_OWNER: usize = 32;

/// How many finished downloads keep their destination (the downloads page opens the file from it); the
/// oldest are forgotten past this, so the bookkeeping is bounded however long the session lasts.
pub const KEEP_FINISHED: usize = 1024;

/// The name used when the server's gives nothing usable.
const FALLBACK_NAME: &str = "download";

/// Where one download goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Destination {
    /// The file the download becomes when it completes.
    pub path: PathBuf,
    /// Where the engine writes it until then.
    pub part: PathBuf,
}

/// Where each download is saved, by id: shared by the engine's thread (which asks for a
/// destination as a download starts) and the window.
pub type Destinations = Arc<Mutex<HashMap<u64, Destination>>>;

/// Which downloads are under way, and in what order the finished ones are forgotten.
#[derive(Default)]
struct Accounts {
    /// Download id to its owner, for the downloads under way.
    active: BTreeMap<u64, Option<u64>>,
    /// Finished downloads, oldest first.
    finished: VecDeque<u64>,
}

/// Chooses the destination of every download, in one folder.
pub struct Saver {
    dir: PathBuf,
    destinations: Destinations,
    accounts: Mutex<Accounts>,
}

impl Saver {
    /// A saver into `dir` (created on the first download).
    pub fn new(dir: PathBuf) -> Saver {
        Saver {
            dir,
            destinations: Destinations::default(),
            accounts: Mutex::default(),
        }
    }

    /// The folder downloads go to.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where the downloads go, for the window.
    pub fn destinations(&self) -> Destinations {
        Arc::clone(&self.destinations)
    }

    /// Reserves a destination for download `id` whose server suggested `suggested`: the folder is
    /// made, the name is made safe and unique among the files there and the downloads under way.
    pub fn reserve(&self, id: u64, suggested: &str) -> io::Result<Destination> {
        self.reserve_as(None, id, suggested)
    }

    /// [`Saver::reserve`] for `owner`, who also may have at most [`MAX_DOWNLOADS_PER_OWNER`] under way.
    pub fn reserve_for(&self, owner: u64, id: u64, suggested: &str) -> io::Result<Destination> {
        self.reserve_as(Some(owner), id, suggested)
    }

    fn reserve_as(&self, owner: Option<u64>, id: u64, suggested: &str) -> io::Result<Destination> {
        fs::create_dir_all(&self.dir)?;
        // The destinations first, then the accounts, everywhere.
        let mut taken = self
            .destinations
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut accounts = self.accounts.lock().unwrap_or_else(PoisonError::into_inner);
        if accounts.active.len() >= MAX_DOWNLOADS {
            return Err(io::Error::other("too many downloads under way"));
        }
        if owner.is_some()
            && accounts.active.values().filter(|o| **o == owner).count() >= MAX_DOWNLOADS_PER_OWNER
        {
            return Err(io::Error::other(
                "too many downloads under way for this app",
            ));
        }
        // Only the downloads under way hold a name: a finished one either left its file in the folder
        // (which `unique_path` sees) or removed it, and then the name is free again. Checking every
        // destination ever kept would also make each new name cost more than the last.
        let path = {
            let busy: HashSet<&Path> = taken
                .iter()
                .filter(|(id, _)| accounts.active.contains_key(*id))
                .flat_map(|(_, d)| [d.path.as_path(), d.part.as_path()])
                .collect();
            unique_path(&self.dir, &sanitize(suggested), |p| busy.contains(p))
        };
        let destination = Destination {
            part: part_path(&path),
            path,
        };
        taken.insert(id, destination.clone());
        accounts.active.insert(id, owner);
        Ok(destination)
    }

    /// The destination of download `id`.
    pub fn destination(&self, id: u64) -> Option<Destination> {
        self.destinations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .cloned()
    }

    /// Download `id` ended: a completed file is renamed from its `.part`; anything else removes
    /// it. The destination stays known (the downloads page opens the file from it).
    pub fn finish(&self, id: u64, completed: bool) -> io::Result<()> {
        let Some(destination) = self.destination(id) else {
            return Ok(());
        };
        self.release(id);
        if completed {
            // Something else may have made the name since it was reserved: never replace it.
            let result = if destination.path.exists() {
                Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "a file of that name appeared meanwhile",
                ))
            } else {
                fs::rename(&destination.part, &destination.path)
            };
            result.inspect_err(|_| {
                let _ = fs::remove_file(&destination.part);
            })
        } else {
            match fs::remove_file(&destination.part) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        }
    }
}

impl Saver {
    /// Download `id` is no longer under way: its place is free again, and the oldest finished
    /// destinations beyond [`KEEP_FINISHED`] are forgotten.
    fn release(&self, id: u64) {
        let mut taken = self
            .destinations
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut accounts = self.accounts.lock().unwrap_or_else(PoisonError::into_inner);
        if accounts.active.remove(&id).is_none() {
            return;
        }
        accounts.finished.push_back(id);
        while accounts.finished.len() > KEEP_FINISHED {
            if let Some(old) = accounts.finished.pop_front() {
                taken.remove(&old);
            }
        }
    }
}

/// A file name fit for the Downloads folder from what a server sent: the
/// last path segment, without control characters, separators or a leading
/// dot, bounded in length. Never empty.
pub fn sanitize(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last
        .chars()
        .map(|c| match c {
            c if c.is_control() => '_',
            // Direction overrides and other invisible format characters spoof an extension
            // (`invoice\u{202E}fdp.exe` reads as `invoice` + `exe.pdf`).
            '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2069}'
            | '\u{FEFF}' => '_',
            ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_start_matches('.').trim();
    let mut out = String::new();
    for c in trimmed.chars() {
        if out.len() + c.len_utf8() > MAX_NAME {
            break;
        }
        out.push(c);
    }
    if out.is_empty() {
        FALLBACK_NAME.to_string()
    } else {
        out
    }
}

/// `dir/name`, or `dir/stem (n).ext` with the first `n` free (counting a
/// download still in progress as taken, and whatever `reserved` says is).
pub fn unique_path(dir: &Path, name: &str, reserved: impl Fn(&Path) -> bool) -> PathBuf {
    let taken = |p: &Path| p.exists() || part_path(p).exists() || reserved(p);
    let first = dir.join(name);
    if !taken(&first) {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !taken(p))
        .expect("an unbounded range finds a free name")
}

/// Where a download is written until it completes.
pub fn part_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lazywebplus-dl-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn server_names_cannot_escape_the_folder() {
        assert_eq!(sanitize("../../etc/passwd"), "passwd");
        assert_eq!(sanitize("C:\\Windows\\evil.exe"), "evil.exe");
        assert_eq!(sanitize(".hidden"), "hidden");
        assert_eq!(sanitize("a\nb:c?.zip"), "a_b_c_.zip");
        assert_eq!(sanitize("  "), "download");
        assert_eq!(sanitize(".."), "download");
        assert_eq!(sanitize(""), "download");
        assert_eq!(sanitize("dir/"), "download");
        assert_eq!(sanitize(&"é".repeat(200)).len(), MAX_NAME);
    }

    #[test]
    fn taken_names_get_a_number() {
        let dir = temp("unique");
        fs::create_dir_all(&dir).unwrap();
        let free = |_: &Path| false;
        assert_eq!(unique_path(&dir, "a.zip", free), dir.join("a.zip"));
        fs::write(dir.join("a.zip"), b"").unwrap();
        fs::write(dir.join("a (1).zip.part"), b"").unwrap();
        assert_eq!(unique_path(&dir, "a.zip", free), dir.join("a (2).zip"));
        fs::write(dir.join("README"), b"").unwrap();
        assert_eq!(unique_path(&dir, "README", free), dir.join("README (1)"));
        let reserved = |p: &Path| p == dir.join("b.zip");
        assert_eq!(unique_path(&dir, "b.zip", reserved), dir.join("b (1).zip"));
    }

    #[test]
    fn two_downloads_of_one_name_get_two_files_before_either_has_a_byte() {
        let dir = temp("twice");
        let saver = Saver::new(dir.clone());
        let first = saver.reserve(1, "files.zip").unwrap();
        let second = saver.reserve(2, "files.zip").unwrap();
        assert_eq!(first.path, dir.join("files.zip"));
        assert_eq!(first.part, dir.join("files.zip.part"));
        assert_eq!(second.path, dir.join("files (1).zip"));
        assert_ne!(first.part, second.part);
        assert_eq!(saver.destination(2), Some(second));
        assert!(
            !first.part.exists(),
            "the engine creates the file; an existing one would be refused as an overwrite"
        );
    }

    #[test]
    fn a_completed_download_is_renamed_and_a_failed_one_removed() {
        let dir = temp("sink");
        let saver = Saver::new(dir.clone());
        let done = saver.reserve(1, "files.zip").unwrap();
        fs::write(&done.part, b"PK").unwrap();
        saver.finish(1, true).unwrap();
        assert_eq!(fs::read(dir.join("files.zip")).unwrap(), b"PK");
        assert!(!done.part.exists());

        let failed = saver.reserve(2, "files.zip").unwrap();
        fs::write(&failed.part, b"P").unwrap();
        saver.finish(2, false).unwrap();
        assert!(!failed.part.exists() && !failed.path.exists());
        // A failure before the engine wrote anything is not an error either.
        saver.reserve(3, "x.bin").unwrap();
        saver.finish(3, false).unwrap();
        // An unknown download is nothing to do.
        saver.finish(99, true).unwrap();
        assert_eq!(saver.destination(1).unwrap().path, dir.join("files.zip"));
    }

    #[test]
    fn invisible_direction_characters_cannot_spoof_an_extension() {
        let name = sanitize("invoice\u{202E}fdp.exe");
        assert!(!name.contains('\u{202E}'), "{name}");
        assert_eq!(name, "invoice_fdp.exe");
        assert_eq!(sanitize("a\u{200B}b\u{FEFF}c"), "a_b_c");
    }

    #[test]
    fn a_page_cannot_start_unlimited_downloads() {
        let saver = Saver::new(temp("cap"));
        for id in 0..MAX_DOWNLOADS as u64 {
            saver.reserve(id, "f.bin").unwrap();
        }
        assert!(saver.reserve(MAX_DOWNLOADS as u64, "f.bin").is_err());
    }

    #[test]
    fn a_finished_download_frees_its_place() {
        // Why (review of 2026-10-22, finding 5): the cap counted every download ever reserved, so
        // after 256 in one run of webd every client's downloads were refused until it restarted.
        let saver = Saver::new(temp("places"));
        for id in 0..MAX_DOWNLOADS as u64 {
            saver.reserve(id, "f.bin").unwrap();
        }
        assert!(saver.reserve(1000, "f.bin").is_err());
        saver.finish(3, false).unwrap();
        saver.finish(3, false).unwrap();
        saver.reserve(1000, "f.bin").unwrap();
        assert!(saver.reserve(1001, "f.bin").is_err(), "one place, once");
        // A completed one frees its place as well.
        let d = saver.destination(7).unwrap();
        fs::write(&d.part, b"x").unwrap();
        saver.finish(7, true).unwrap();
        saver.reserve(1002, "h.bin").unwrap();
    }

    #[test]
    fn a_long_session_never_runs_out_of_places() {
        let saver = Saver::new(temp("long"));
        for id in 0..(3 * MAX_DOWNLOADS) as u64 {
            saver.reserve(id, "f.bin").unwrap();
            saver.finish(id, false).unwrap();
        }
    }

    #[test]
    fn one_owner_cannot_use_up_anothers_places() {
        let saver = Saver::new(temp("owners"));
        for id in 0..MAX_DOWNLOADS_PER_OWNER as u64 {
            saver.reserve_for(1, id, "f.bin").unwrap();
        }
        assert!(saver.reserve_for(1, 1000, "f.bin").is_err());
        let other = saver.reserve_for(2, 2000, "f.bin").unwrap();
        assert!(other.path.starts_with(saver.dir()));
        saver.finish(0, false).unwrap();
        saver.reserve_for(1, 1000, "f.bin").unwrap();
        // The global cap still holds for all owners together.
        let busy = Saver::new(temp("owners-global"));
        for owner in 0..(MAX_DOWNLOADS / MAX_DOWNLOADS_PER_OWNER) as u64 {
            for n in 0..MAX_DOWNLOADS_PER_OWNER as u64 {
                busy.reserve_for(owner, owner * 1000 + n, "f.bin").unwrap();
            }
        }
        assert!(busy.reserve_for(99, 99_000, "f.bin").is_err());
    }

    #[test]
    fn finished_downloads_are_remembered_a_while_and_then_forgotten() {
        let saver = Saver::new(temp("keep"));
        let total = KEEP_FINISHED as u64 + 50;
        for id in 0..total {
            saver.reserve(id, "f.bin").unwrap();
            saver.finish(id, false).unwrap();
        }
        // The downloads page can still open a recent one...
        assert!(saver.destination(total - 1).is_some());
        // ...and the oldest are gone: the bookkeeping is bounded.
        assert!(saver.destination(0).is_none());
        assert!(saver.destinations().lock().unwrap().len() <= KEEP_FINISHED);
    }

    #[test]
    fn a_file_that_appeared_meanwhile_is_never_replaced() {
        let dir = temp("appeared");
        let saver = Saver::new(dir.clone());
        let d = saver.reserve(1, "files.zip").unwrap();
        fs::write(&d.part, b"new").unwrap();
        fs::write(dir.join("files.zip"), b"theirs").unwrap();
        assert!(saver.finish(1, true).is_err());
        assert_eq!(fs::read(dir.join("files.zip")).unwrap(), b"theirs");
        assert!(!d.part.exists());
    }

    #[test]
    fn a_name_that_is_another_downloads_part_file_is_not_reused() {
        let dir = temp("partclash");
        let saver = Saver::new(dir.clone());
        let first = saver.reserve(1, "a.zip").unwrap();
        let second = saver.reserve(2, "a.zip.part").unwrap();
        assert_ne!(second.path, first.part);
        assert_ne!(second.part, first.part);
    }

    #[test]
    fn a_completed_download_whose_file_is_missing_reports_it() {
        let dir = temp("missing");
        let saver = Saver::new(dir.clone());
        saver.reserve(1, "gone.bin").unwrap();
        assert!(saver.finish(1, true).is_err());
    }
}
