//! The journal store: buffered appends, rotation and the budget over a tiny
//! filesystem abstraction.
//!
//! `logd` binds [`JournalFs`] to `/logs` through its syscalls; the kernel test
//! suite binds it to an ext2 volume through the VFS, and the host tests to a
//! map. Any filesystem error is returned to the caller, which stops using the
//! store (the in-memory ring keeps every record); nothing here retries.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;

use crate::line::{boot_detail, clip, format_line, record_hash, BOOT_TOPIC, MAX_LINE};
use crate::rotate::{file_name, parse_any, Action, Ledger};
use crate::source::{owned_source, source_of, SYSTEM};

/// Records buffered before a flush.
pub const FLUSH_RECORDS: u64 = 32;
/// Ticks a buffered record may wait before a flush.
pub const FLUSH_TICKS: u64 = 100;
/// Most sources with their own journal; a record of a further, new source
/// goes to `system.log`, so hostile topics cannot fill `/logs` with files.
pub const MAX_SOURCES: usize = 64;
/// Room reserved for a boot line when planning an append: a boot line with a
/// continuation hash and its leading newline is well under it.
const BOOT_RESERVE: u64 = 96;

/// The file operations the store is built from, all inside one directory.
pub trait JournalFs {
    /// A filesystem-specific error, returned unchanged.
    type Error: Copy + core::fmt::Debug;

    /// Every regular file in the directory, with its size.
    fn list(&mut self) -> Result<Vec<(String, u64)>, Self::Error>;
    /// Append `data` to `name`, creating it when absent.
    fn append(&mut self, name: &str, data: &[u8]) -> Result<(), Self::Error>;
    /// Rename `from` to `to`; `to` does not exist.
    fn rename(&mut self, from: &str, to: &str) -> Result<(), Self::Error>;
    /// Remove `name`; a missing file is not an error.
    fn remove(&mut self, name: &str) -> Result<(), Self::Error>;
    /// Make every completed append durable.
    fn sync(&mut self) -> Result<(), Self::Error>;
    /// Replace `out` with the contents of `name`; `false` when it does not
    /// exist. `out` is reused, so an implementation fills it in place.
    fn read(&mut self, name: &str, out: &mut Vec<u8>) -> Result<bool, Self::Error>;
}

/// One source's state for this boot.
#[derive(Default)]
struct Journal {
    /// Lines buffered for the live file.
    pending: Vec<u8>,
    /// Records in `pending`.
    pending_records: u64,
    /// Hash of the newest line written or buffered for the live file.
    head: u64,
    /// Whether the live file has this boot's boot line.
    started: bool,
    /// The chain head a rotation left for the next boot line (`0` = none).
    cont: u64,
}

/// What [`Store::tail`] refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TailError<E> {
    /// Not a valid source name.
    Invalid,
    /// No journal for that source.
    Missing,
    /// The filesystem failed.
    Fs(E),
}

/// Persistent journals under one directory.
pub struct Store<F: JournalFs> {
    fs: F,
    boot_id: u64,
    ledger: Ledger,
    journals: BTreeMap<String, Journal>,
    pending_records: u64,
    last_flush: u64,
    persisted: u64,
    read_buffer: Vec<u8>,
}

impl<F: JournalFs> Store<F> {
    /// Open the directory `fs` stands for: list it so the sizes of earlier
    /// boots' journals count towards the caps and the budget.
    pub fn open(mut fs: F, boot_id: u64, now: u64) -> Result<Store<F>, F::Error> {
        let mut ledger = Ledger::new();
        for (name, size) in fs.list()? {
            ledger.insert(&name, size);
        }
        Ok(Store {
            fs,
            boot_id,
            ledger,
            journals: BTreeMap::new(),
            pending_records: 0,
            last_flush: now,
            persisted: 0,
            read_buffer: Vec::new(),
        })
    }

    /// Records written to disk this boot.
    pub fn persisted(&self) -> u64 {
        self.persisted
    }

    /// Records buffered and not yet written.
    pub fn pending(&self) -> u64 {
        self.pending_records
    }

    /// The tracked sizes (for tests and diagnostics).
    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// Give back the filesystem binding.
    pub fn into_inner(self) -> F {
        self.fs
    }

    /// Buffer one record (flushing when [`FLUSH_RECORDS`] are buffered).
    /// `seq` and `tick` are the ring's; the line's hash chains the journal.
    pub fn append(
        &mut self,
        seq: u64,
        tick: u64,
        topic: &str,
        detail: &str,
    ) -> Result<(), F::Error> {
        let source = self.admit(source_of(topic));
        let room = MAX_LINE.saturating_sub(64 + topic.len() * 2);
        let detail = clip(detail, room);
        let estimate = format_line(seq, tick, topic, &detail, 0).len() as u64;
        for action in self.ledger.plan(&source, estimate + BOOT_RESERVE) {
            self.carry_out(&action)?;
        }
        let live = self.ledger.footprint(&source).live();
        let journal = self.journals.entry(source.clone()).or_default();
        let mut text = String::new();
        if !journal.started {
            // A file this boot did not start may end in a line a crash cut
            // short; the newline closes it (see `line::verify`).
            if live > 0 && journal.cont == 0 {
                text.push('\n');
            }
            let detail = boot_detail(self.boot_id, journal.cont);
            let hash = record_hash(journal.cont, 0, tick, BOOT_TOPIC, &detail);
            text.push_str(&format_line(0, tick, BOOT_TOPIC, &detail, hash));
            journal.head = hash;
            journal.started = true;
            journal.cont = 0;
        }
        journal.head = record_hash(journal.head, seq, tick, topic, &detail);
        text.push_str(&format_line(seq, tick, topic, &detail, journal.head));
        journal.pending.extend_from_slice(text.as_bytes());
        journal.pending_records += 1;
        self.ledger.grow(&source, text.len() as u64);
        self.pending_records += 1;
        if self.pending_records >= FLUSH_RECORDS {
            self.flush(tick)?;
        }
        Ok(())
    }

    /// Flush when a buffered record has waited [`FLUSH_TICKS`].
    pub fn tick(&mut self, now: u64) -> Result<(), F::Error> {
        if self.pending_records > 0 && now.saturating_sub(self.last_flush) >= FLUSH_TICKS {
            self.flush(now)?;
        }
        Ok(())
    }

    /// Write every buffered line.
    pub fn flush(&mut self, now: u64) -> Result<(), F::Error> {
        let names: Vec<String> = self.journals.keys().cloned().collect();
        for source in names {
            self.flush_source(&source)?;
        }
        self.last_flush = now;
        Ok(())
    }

    /// Flush, then make it durable (`logd`'s `Shutdown`).
    pub fn sync(&mut self, now: u64) -> Result<(), F::Error> {
        self.flush(now)?;
        self.fs.sync()
    }

    /// Every source with a journal on disk (`pkg` included), sorted.
    pub fn sources(&mut self) -> Result<Vec<String>, F::Error> {
        let mut sources: Vec<String> = self
            .fs
            .list()?
            .iter()
            .filter_map(|(name, _)| parse_any(name))
            .filter(|&(_, generation)| generation == 0)
            .map(|(source, _)| String::from(source))
            .collect();
        sources.sort();
        sources.dedup();
        Ok(sources)
    }

    /// The newest lines of `source`'s journal, oldest first: at most `count`
    /// lines and `max_bytes` bytes, reaching into `.log.1` when the live file
    /// is short. Buffered lines are flushed first so they are included.
    pub fn tail(
        &mut self,
        source: &str,
        count: usize,
        max_bytes: usize,
    ) -> Result<Vec<String>, TailError<F::Error>> {
        if !crate::source::valid_source(source) {
            return Err(TailError::Invalid);
        }
        if self.journals.contains_key(source) {
            self.flush_source(source).map_err(TailError::Fs)?;
        }
        let mut lines: VecDeque<String> = VecDeque::new();
        let mut found = false;
        for generation in [1, 0] {
            let name = file_name(source, generation);
            if !self
                .fs
                .read(&name, &mut self.read_buffer)
                .map_err(TailError::Fs)?
            {
                continue;
            }
            found = true;
            let text = String::from_utf8_lossy(&self.read_buffer);
            for line in text.split('\n').filter(|line| !line.is_empty()) {
                if lines.len() == count {
                    lines.pop_front();
                }
                if count > 0 {
                    lines.push_back(String::from(line));
                }
            }
        }
        if !found {
            return Err(TailError::Missing);
        }
        let mut bytes: usize = lines.iter().map(String::len).sum();
        while bytes > max_bytes {
            bytes -= lines.pop_front().map_or(0, |line| line.len());
        }
        Ok(lines.into_iter().collect())
    }

    /// The journal a record of `source` goes to: `system` once
    /// [`MAX_SOURCES`] journals exist.
    fn admit(&self, source: &str) -> String {
        let known = self.ledger.contains(source) || self.journals.contains_key(source);
        if known || self.ledger.len() < MAX_SOURCES {
            String::from(source)
        } else {
            String::from(SYSTEM)
        }
    }

    /// Write one source's buffered lines.
    fn flush_source(&mut self, source: &str) -> Result<(), F::Error> {
        let Some(journal) = self.journals.get_mut(source) else {
            return Ok(());
        };
        if journal.pending.is_empty() {
            return Ok(());
        }
        self.fs.append(&file_name(source, 0), &journal.pending)?;
        journal.pending.clear();
        self.persisted += journal.pending_records;
        self.pending_records -= journal.pending_records;
        journal.pending_records = 0;
        Ok(())
    }

    /// Carry out one step of [`Ledger::plan`].
    fn carry_out(&mut self, action: &Action) -> Result<(), F::Error> {
        match action {
            Action::Rotate(source) => {
                debug_assert!(owned_source(source));
                // The buffered lines belong at the end of the file that is
                // about to become `.1`.
                self.flush_source(source)?;
                let sizes = self.ledger.footprint(source);
                self.fs.remove(&file_name(source, 2))?;
                if sizes.0[1] > 0 {
                    self.fs
                        .rename(&file_name(source, 1), &file_name(source, 2))?;
                } else {
                    self.fs.remove(&file_name(source, 1))?;
                }
                if sizes.0[0] > 0 {
                    self.fs
                        .rename(&file_name(source, 0), &file_name(source, 1))?;
                }
                if let Some(journal) = self.journals.get_mut(source) {
                    // The next file continues this boot's chain.
                    journal.cont = if journal.started { journal.head } else { 0 };
                    journal.started = false;
                }
            }
            Action::Remove(source, generation) => {
                self.fs.remove(&file_name(source, *generation))?;
            }
        }
        self.ledger.apply(action);
        Ok(())
    }
}
