//! Caps, rotation and the `/logs` budget, as pure arithmetic.
//!
//! Every source owns up to three files: `<source>.log` (live), `.log.1` and
//! `.log.2`. The [`Ledger`] tracks their sizes (what is on disk plus what is
//! buffered for it) and [`Ledger::plan`] says what has to happen before an
//! append of `n` bytes so that
//!
//! * no live file grows past [`FILE_CAP`]: the source is rotated first
//!   (`.2` deleted, `.1` -> `.2`, live -> `.1`);
//! * the sum over every owned file stays within [`BUDGET`]: the largest
//!   source (by its three files together) gives up its oldest generation
//!   first, and a source with only a live file is rotated so its next turn
//!   frees it.
//!
//! Files `logd` does not own (`pkg.log`, anything not named like a journal)
//! are not in the ledger, so they are neither counted nor touched.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::source::owned_source;

/// Largest live journal file.
pub const FILE_CAP: u64 = 256 * 1024;
/// Largest total of every file `logd` owns under `/logs`.
pub const BUDGET: u64 = 8 * 1024 * 1024;
/// Files per source: the live one and two rotations.
pub const GENERATIONS: usize = 3;

/// File name of `source`'s generation `generation` (`0` = live).
pub fn file_name(source: &str, generation: usize) -> String {
    match generation {
        0 => format!("{source}.log"),
        n => format!("{source}.log.{n}"),
    }
}

/// `(source, generation)` for a journal file name, whatever its owner.
pub fn parse_any(name: &str) -> Option<(&str, usize)> {
    if let Some(source) = name.strip_suffix(".log") {
        return crate::source::valid_source(source).then_some((source, 0));
    }
    let (stem, generation) = name.rsplit_once('.')?;
    let generation = match generation {
        "1" => 1,
        "2" => 2,
        _ => return None,
    };
    let source = stem.strip_suffix(".log")?;
    crate::source::valid_source(source).then_some((source, generation))
}

/// `(source, generation)` for a file `logd` owns, `None` for anything else.
pub fn parse_name(name: &str) -> Option<(&str, usize)> {
    parse_any(name).filter(|(source, _)| owned_source(source))
}

/// One source's three file sizes, live first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Footprint(pub [u64; GENERATIONS]);

impl Footprint {
    pub fn total(&self) -> u64 {
        self.0.iter().sum()
    }

    pub fn live(&self) -> u64 {
        self.0[0]
    }

    /// The sizes after a rotation.
    pub fn rotated(&self) -> Footprint {
        Footprint([0, self.0[0], self.0[1]])
    }

    /// The oldest generation holding bytes, `None` when only the live file
    /// does (or nothing does).
    pub fn oldest(&self) -> Option<usize> {
        (1..GENERATIONS)
            .rev()
            .find(|&generation| self.0[generation] > 0)
    }
}

/// One step [`Ledger::plan`] asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Rotate the source: `.2` deleted, `.1` -> `.2`, live -> `.1`.
    Rotate(String),
    /// Delete one rotated generation of the source.
    Remove(String, usize),
}

/// The sizes of every owned journal file, and their total.
#[derive(Clone, Debug, Default)]
pub struct Ledger {
    files: BTreeMap<String, Footprint>,
    total: u64,
}

impl Ledger {
    pub fn new() -> Ledger {
        Ledger::default()
    }

    /// Record one file from a directory listing; foreign names are ignored.
    pub fn insert(&mut self, name: &str, size: u64) {
        if let Some((source, generation)) = parse_name(name) {
            let entry = self.files.entry(String::from(source)).or_default();
            self.total = self.total - entry.0[generation] + size;
            entry.0[generation] = size;
        }
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    pub fn footprint(&self, source: &str) -> Footprint {
        self.files.get(source).copied().unwrap_or_default()
    }

    pub fn contains(&self, source: &str) -> bool {
        self.files.contains_key(source)
    }

    /// Owned sources with at least one file, in name order.
    pub fn sources(&self) -> impl Iterator<Item = (&str, Footprint)> {
        self.files
            .iter()
            .map(|(name, sizes)| (name.as_str(), *sizes))
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// `bytes` were appended to (or buffered for) `source`'s live file.
    pub fn grow(&mut self, source: &str, bytes: u64) {
        let entry = self.files.entry(String::from(source)).or_default();
        entry.0[0] += bytes;
        self.total += bytes;
    }

    /// Reflect a step that was carried out.
    pub fn apply(&mut self, action: &Action) {
        let (source, after) = match action {
            Action::Rotate(source) => (source, self.footprint(source).rotated()),
            Action::Remove(source, generation) => {
                let mut sizes = self.footprint(source);
                sizes.0[*generation] = 0;
                (source, sizes)
            }
        };
        let before = self.footprint(source).total();
        self.total = self.total - before + after.total();
        self.files.insert(source.clone(), after);
    }

    /// Whether an append of `bytes` to `source` needs no step at all.
    pub fn fits(&self, source: &str, bytes: u64) -> bool {
        self.footprint(source).live() + bytes <= FILE_CAP && self.total + bytes <= BUDGET
    }

    /// The steps that make room for `bytes` more in `source`'s live file,
    /// in order. Empty when the append already fits. `bytes` larger than
    /// [`FILE_CAP`] cannot fit any file; the caller clips records well below
    /// it.
    pub fn plan(&self, source: &str, bytes: u64) -> Vec<Action> {
        let mut actions = Vec::new();
        if self.fits(source, bytes) {
            return actions;
        }
        let mut model = self.clone();
        if model.footprint(source).live() > 0 && model.footprint(source).live() + bytes > FILE_CAP {
            let action = Action::Rotate(String::from(source));
            model.apply(&action);
            actions.push(action);
        }
        while model.total + bytes > BUDGET {
            let Some(action) = model.shed() else {
                break;
            };
            model.apply(&action);
            actions.push(action);
        }
        actions
    }

    /// The step that frees bytes from the largest source: its oldest
    /// rotated generation, or a rotation when it has only a live file.
    fn shed(&self) -> Option<Action> {
        // Largest footprint first; ties go to the first name, so the choice
        // is deterministic.
        let (source, sizes) = self
            .files
            .iter()
            .filter(|(_, sizes)| sizes.total() > 0)
            .fold(
                None::<(&String, Footprint)>,
                |best, (name, sizes)| match best {
                    Some((_, top)) if top.total() >= sizes.total() => best,
                    _ => Some((name, *sizes)),
                },
            )?;
        Some(match sizes.oldest() {
            Some(generation) => Action::Remove(source.clone(), generation),
            None => Action::Rotate(source.clone()),
        })
    }
}
