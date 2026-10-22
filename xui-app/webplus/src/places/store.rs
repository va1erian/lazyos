//! The bookmarks: the entries in memory and the log file that keeps them.
//!
//! Every change is a few [`Op`]s, applied in memory and then appended to the file and synced (a
//! change is one commit). A change that cannot be written is undone in memory and says so
//! ([`Error::Io`]), so the set always matches the file. A write that fails partway is cut back to
//! the length the file had; if that fails too the file ends in torn bytes that would hide every
//! later record, so the store writes nothing but a rewrite of the whole file until one succeeds
//! (every change tries it). When more than half of the file's records are dead (replaced or
//! deleted entries) it is rewritten into a new file that is renamed over the old one.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use super::file::{append, clean_web_url, read_head, truncate, Fault};
use super::log::{self, Op};
use super::model::{
    clean_title, clean_url, is_root, Entry, Error, Id, Kind, FIRST_ID, MAX_DEPTH, MAX_ENTRIES,
    UNSORTED,
};

/// A file with fewer records is never rewritten: compaction would cost more than it saves.
pub const COMPACT_MIN_RECORDS: usize = 32;

/// The set of bookmarks and the file behind it (none for a set that is not saved).
#[derive(Debug)]
pub struct Bookmarks {
    entries: BTreeMap<Id, Entry>,
    file: Option<PathBuf>,
    /// Records in the file, live and dead.
    records: usize,
    next_id: Id,
    /// Counts the changes made, so a view can tell that it is out of date.
    version: u64,
    /// The file ends in bytes that could not be cut off: only a rewrite makes it appendable again.
    unwritable: bool,
    /// Failures to inject into the next write (tests only set it).
    pub(super) fault: Fault,
}

impl Bookmarks {
    /// Bookmarks kept only in memory.
    pub fn in_memory() -> Bookmarks {
        Bookmarks {
            entries: BTreeMap::new(),
            file: None,
            records: 0,
            next_id: FIRST_ID,
            version: 0,
            unwritable: false,
            fault: Fault::default(),
        }
    }

    /// The bookmarks saved in `file` (made on the first change). A missing or unreadable file is
    /// an empty set; what cannot be read from a damaged one is dropped, and the file is rewritten
    /// so that the next change is not appended after the damage. A file that is not a bookmarks log
    /// is renamed `<file>.bad` and left alone.
    pub fn open(file: PathBuf) -> Bookmarks {
        let mut set = Bookmarks::in_memory();
        let bytes = read_head(&file);
        let parsed = log::parse(&bytes);
        if parsed.foreign {
            let _ = fs::rename(&file, file.with_extension("log.bad"));
            set.file = Some(file);
            return set;
        }
        for op in parsed.ops {
            set.apply_loaded(op);
        }
        set.repair_tree();
        set.records = parsed.records;
        set.file = Some(file.clone());
        let file_len = fs::metadata(&file).map_or(0, |m| m.len());
        if (parsed.valid_len as u64) < file_len
            && set.compact().is_err()
            && truncate(&file, parsed.valid_len).is_err()
        {
            // Appending after the damage would write records no later read can reach.
            set.file = None;
        }
        set
    }

    /// How many bookmarks, folders and separators there are (not counting the roots).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// An entry of the person's (a root has none).
    pub fn get(&self, id: Id) -> Option<&Entry> {
        self.entries.get(&id)
    }

    /// Every entry, by number.
    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    /// What is in the root or folder `parent`, in order.
    pub fn children(&self, parent: Id) -> Vec<&Entry> {
        let mut found: Vec<&Entry> = self
            .entries
            .values()
            .filter(|e| e.parent == parent)
            .collect();
        found.sort_by_key(|e| (e.position, e.id));
        found
    }

    /// The oldest link to exactly `url`.
    pub fn find_url(&self, url: &str) -> Option<&Entry> {
        self.entries
            .values()
            .find(|e| e.kind == Kind::Link && e.url == url)
    }

    /// A number that changes with every change to the set.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Records in the file, dead ones included.
    pub fn records(&self) -> usize {
        self.records
    }

    /// Adds a link to the root or folder `parent`, last.
    pub fn add_link(&mut self, parent: Id, title: &str, url: &str, now: u64) -> Result<Id, Error> {
        let url = clean_web_url(url)?;
        self.add(parent, Kind::Link, clean_title(title), url, now)
    }

    /// Adds a folder to `parent`, last.
    pub fn add_folder(&mut self, parent: Id, title: &str, now: u64) -> Result<Id, Error> {
        if self.depth_of(parent).is_none_or(|d| d + 1 >= MAX_DEPTH) {
            return Err(Error::BadParent);
        }
        self.add(parent, Kind::Folder, clean_title(title), String::new(), now)
    }

    /// Adds a separator to `parent`, last.
    pub fn add_separator(&mut self, parent: Id, now: u64) -> Result<Id, Error> {
        self.add(parent, Kind::Separator, String::new(), String::new(), now)
    }

    fn add(
        &mut self,
        parent: Id,
        kind: Kind,
        title: String,
        url: String,
        now: u64,
    ) -> Result<Id, Error> {
        if self.entries.len() >= MAX_ENTRIES {
            return Err(Error::Full);
        }
        if !self.is_container(parent) {
            return Err(Error::BadParent);
        }
        let position = self
            .entries
            .values()
            .filter(|e| e.parent == parent)
            .map(|e| e.position.saturating_add(1))
            .max()
            .unwrap_or(0);
        let before = self.snapshot();
        let id = self.next_id;
        self.next_id += 1;
        let entry = Entry {
            id,
            parent,
            kind,
            title,
            url,
            position,
            created: now,
            modified: now,
        };
        self.entries.insert(id, entry.clone());
        self.commit(before, &[Op::Put(entry)])?;
        Ok(id)
    }

    /// Changes a link's title and address, or a folder's title (its `url` is ignored).
    pub fn edit(&mut self, id: Id, title: &str, url: &str, now: u64) -> Result<(), Error> {
        let mut entry = self.entries.get(&id).cloned().ok_or(Error::NoSuchEntry)?;
        match entry.kind {
            Kind::Separator => return Err(Error::NoSuchEntry),
            Kind::Link => entry.url = clean_web_url(url)?,
            Kind::Folder => {}
        }
        entry.title = clean_title(title);
        entry.modified = now;
        let before = self.snapshot();
        self.entries.insert(id, entry.clone());
        self.commit(before, &[Op::Put(entry)])
    }

    /// Moves `id` into `parent` at `index` among its entries (past the end is last).
    pub fn move_to(&mut self, id: Id, parent: Id, index: usize, now: u64) -> Result<(), Error> {
        let moving = self.entries.get(&id).ok_or(Error::NoSuchEntry)?;
        let height = if moving.kind == Kind::Folder {
            self.height_of(id)
        } else {
            0
        };
        let fits = self
            .depth_of(parent)
            .is_some_and(|d| d + 1 + height < MAX_DEPTH);
        if !self.is_container(parent) || self.is_inside(parent, id) || !fits {
            return Err(Error::BadParent);
        }
        let mut order: Vec<Id> = self.children(parent).iter().map(|e| e.id).collect();
        order.retain(|other| *other != id);
        order.insert(index.min(order.len()), id);
        let before = self.snapshot();
        let mut ops = Vec::new();
        for (position, other) in order.into_iter().enumerate() {
            let position = u32::try_from(position).unwrap_or(u32::MAX);
            let Some(entry) = self.entries.get_mut(&other) else {
                continue;
            };
            if entry.position != position || entry.parent != parent {
                entry.position = position;
                entry.parent = parent;
                entry.modified = now;
                ops.push(Op::Put(entry.clone()));
            }
        }
        // Where it left from keeps its gap: positions only order, they need not be dense.
        self.commit(before, &ops)
    }

    /// Removes `id` and, for a folder, everything in it; how many entries went.
    pub fn remove(&mut self, id: Id) -> Result<usize, Error> {
        if !self.entries.contains_key(&id) {
            return Err(Error::NoSuchEntry);
        }
        let mut gone = vec![id];
        let mut next = 0;
        while next < gone.len() {
            let parent = gone[next];
            next += 1;
            gone.extend(
                self.entries
                    .values()
                    .filter(|e| e.parent == parent)
                    .map(|e| e.id),
            );
        }
        let ops: Vec<Op> = gone.iter().map(|id| Op::Delete(*id)).collect();
        let before = self.snapshot();
        for id in &gone {
            self.entries.remove(id);
        }
        self.commit(before, &ops)?;
        Ok(gone.len())
    }

    /// Whether `id` is a root or a folder: somewhere an entry can go.
    fn is_container(&self, id: Id) -> bool {
        is_root(id)
            || self
                .entries
                .get(&id)
                .is_some_and(|e| e.kind == Kind::Folder)
    }

    /// How many folders are above `id` (0 for a root), or `None` when `id` is not in the tree.
    fn depth_of(&self, id: Id) -> Option<usize> {
        let mut depth = 0;
        let mut at = id;
        while !is_root(at) {
            at = self.entries.get(&at)?.parent;
            depth += 1;
            if depth > MAX_DEPTH {
                return None;
            }
        }
        Some(depth)
    }

    /// How many levels of folders lie below `id` (0 for one without a folder in it).
    fn height_of(&self, id: Id) -> usize {
        let below = |folder: Id| {
            self.entries
                .values()
                .filter(move |e| e.parent == folder && e.kind == Kind::Folder)
        };
        let mut level: Vec<Id> = below(id).map(|e| e.id).collect();
        let mut height = 0;
        while !level.is_empty() && height <= MAX_DEPTH {
            height += 1;
            level = level.iter().flat_map(|f| below(*f).map(|e| e.id)).collect();
        }
        height
    }

    /// Whether `id` is `ancestor` or lies below it.
    fn is_inside(&self, id: Id, ancestor: Id) -> bool {
        let mut at = id;
        for _ in 0..=MAX_DEPTH {
            if at == ancestor {
                return true;
            }
            match self.entries.get(&at) {
                Some(e) => at = e.parent,
                None => return false,
            }
        }
        true
    }

    /// Applies an operation read from the file: nothing in a file is trusted.
    fn apply_loaded(&mut self, op: Op) {
        match op {
            Op::Put(mut e) => {
                if e.id < FIRST_ID || e.parent == e.id {
                    return;
                }
                if !self.entries.contains_key(&e.id) && self.entries.len() >= MAX_ENTRIES {
                    return;
                }
                e.title = clean_title(&e.title);
                match e.kind {
                    // Only web pages are ever opened from the bookmarks (`bookmarkable`): a file
                    // that holds another scheme was not written by this browser.
                    Kind::Link => match clean_url(&e.url) {
                        Ok(url) if crate::bookmarks::bookmarkable(&url) => e.url = url,
                        // The file's last word on this id is a refusal, so an earlier good record
                        // of it does not survive either.
                        _ => {
                            self.entries.remove(&e.id);
                            return;
                        }
                    },
                    Kind::Folder => e.url.clear(),
                    Kind::Separator => {
                        e.url.clear();
                        e.title.clear();
                    }
                }
                self.next_id = self.next_id.max(e.id.saturating_add(1));
                self.entries.insert(e.id, e);
            }
            Op::Delete(id) => {
                self.entries.remove(&id);
            }
        }
    }

    /// After loading: an entry whose way up does not reach a root (its parent is missing or not a
    /// folder, a loop, nested deeper than [`MAX_DEPTH`]) goes to Unsorted: nothing is dropped for
    /// being misfiled.
    fn repair_tree(&mut self) {
        let ids: Vec<Id> = self.entries.keys().copied().collect();
        // First the entry at which the way up breaks, so the ones below it stay with it...
        for id in &ids {
            let mut at = *id;
            for _ in 0..=MAX_DEPTH {
                let Some(parent) = self.entries.get(&at).map(|e| e.parent) else {
                    break;
                };
                if is_root(parent) {
                    break;
                }
                if !self.is_container(parent) {
                    self.rehome(at);
                    break;
                }
                at = parent;
            }
        }
        // ...then whatever is still too deep or looped.
        for id in ids {
            if self.depth_of(id).is_none() {
                self.rehome(id);
            }
        }
    }

    fn rehome(&mut self, id: Id) {
        if let Some(e) = self.entries.get_mut(&id) {
            e.parent = UNSORTED;
        }
    }

    /// The entries as they are now, kept to undo a change that cannot be written (none for a set
    /// that is not saved: nothing can fail there).
    fn snapshot(&self) -> Option<BTreeMap<Id, Entry>> {
        self.file.is_some().then(|| self.entries.clone())
    }

    /// Writes `ops` to the file as one commit, and compacts when the file is mostly dead. When it
    /// cannot be written the entries go back to `before`.
    fn commit(&mut self, before: Option<BTreeMap<Id, Entry>>, ops: &[Op]) -> Result<(), Error> {
        if ops.is_empty() {
            return Ok(());
        }
        let Some(file) = self.file.clone() else {
            self.records += ops.len();
            self.version += 1;
            return Ok(());
        };
        let written = if self.unwritable {
            // The rewrite carries this change too: it is made from the entries as they now are.
            self.compact().map_err(|e| (e, false))
        } else {
            let mut bytes = Vec::new();
            for op in ops {
                bytes.extend_from_slice(&log::encode(op));
            }
            append(&file, &bytes, self.fault).map(|()| self.records += ops.len())
        };
        match written {
            Ok(()) => {
                self.unwritable = false;
                self.version += 1;
                if self.should_compact() {
                    // Best effort: the log is whole either way.
                    let _ = self.compact();
                }
                Ok(())
            }
            Err((error, torn)) => {
                if let Some(before) = before {
                    self.entries = before;
                }
                self.unwritable |= torn;
                Err(Error::Io(error))
            }
        }
    }

    fn should_compact(&self) -> bool {
        self.records >= COMPACT_MIN_RECORDS
            && self.records.saturating_sub(self.entries.len()) * 2 > self.records
    }

    /// Rewrites the file with one record per live entry, into a new file renamed over the old one.
    pub fn compact(&mut self) -> io::Result<()> {
        let Some(file) = self.file.clone() else {
            return Ok(());
        };
        let mut bytes = log::MAGIC.to_vec();
        for e in self.entries.values() {
            bytes.extend_from_slice(&log::encode(&Op::Put(e.clone())));
        }
        let temp = file.with_extension("log.tmp");
        let written = (|| {
            let mut f = fs::File::create(&temp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
            fs::rename(&temp, &file)
        })();
        if written.is_err() {
            let _ = fs::remove_file(&temp);
        }
        written?;
        self.records = self.entries.len();
        Ok(())
    }
}
