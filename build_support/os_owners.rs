//! Who put each file in the image (docs/update-plan.md, P0 and decision 16).
//!
//! Every file of the OS volume reaches [`OsFiles`](crate::os_image::OsFiles)
//! through one [`Sink`](crate::os_image::Sink), and a later entry for a path
//! used to replace the earlier one silently. The build now names the code that
//! is adding files (`files.producer("xui_embed")`) and this module remembers,
//! per path, the producer that owns it. When a second *producer* adds a path
//! another one already placed, that is a [`Collision`]: with system packages a
//! file has exactly one owning package, so it must be settled, not overwritten.
//! A producer replacing its own path (an autostart variant of a package, say)
//! is not one.
//!
//! The table is written to `target/os-owners.tsv` on every build, and
//! `LAZYOS_OWNER_STRICT=1` (CI) turns a collision into a build failure.

// `lazyos-install` and the host tests include this file and use only part of it.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use crate::os_image::OsFiles;

/// The producer of files added before any `producer` call: the inline
/// `files.add_file(...)` lines of `build.rs`.
pub const DEFAULT_PRODUCER: &str = "build.rs";

/// A path two different producers both placed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collision {
    pub path: String,
    /// In the order they placed it; the last one's bytes are in the image.
    pub producers: Vec<&'static str>,
}

/// The owner of every placed path.
#[derive(Debug)]
pub struct Owners {
    current: &'static str,
    // Path to the producers that placed it, in order, without repeats of the
    // one before.
    placed: BTreeMap<String, Vec<&'static str>>,
}

impl Default for Owners {
    fn default() -> Self {
        Owners {
            current: DEFAULT_PRODUCER,
            placed: BTreeMap::new(),
        }
    }
}

impl Owners {
    /// Everything added from now on belongs to `producer`.
    pub fn producer(&mut self, producer: &'static str) -> &'static str {
        std::mem::replace(&mut self.current, producer)
    }

    /// Record that the current producer placed `path` (already cleaned).
    pub fn note(&mut self, path: &str) {
        let producers = self.placed.entry(path.to_string()).or_default();
        if producers.last() != Some(&self.current) {
            producers.push(self.current);
        }
    }

    /// The owner of `path`: the producer whose bytes the image holds.
    pub fn owner(&self, path: &str) -> Option<&'static str> {
        self.placed.get(path).and_then(|p| p.last().copied())
    }

    /// Paths placed by more than one producer.
    pub fn collisions(&self) -> Vec<Collision> {
        self.placed
            .iter()
            .filter(|(_, producers)| producers.len() > 1)
            .map(|(path, producers)| Collision {
                path: path.clone(),
                producers: producers.clone(),
            })
            .collect()
    }

    /// `path<TAB>owner` per line, sorted by path.
    pub fn table(&self) -> String {
        let mut out = String::new();
        for (path, producers) in &self.placed {
            let owner = producers.last().copied().unwrap_or(DEFAULT_PRODUCER);
            let _ = writeln!(out, "{path}\t{owner}");
        }
        out
    }

    /// One line per collision, for a warning or a failure.
    pub fn describe_collisions(&self) -> Vec<String> {
        self.collisions()
            .into_iter()
            .map(|c| format!("{} is placed by {}", c.path, c.producers.join(" then ")))
            .collect()
    }
}

/// `produced!(files, module::function, args...)` calls
/// `module::function(&mut files, args...)` with every file it adds owned by
/// `module`.
#[macro_export]
macro_rules! produced {
    ($files:expr, $module:ident :: $function:ident $(, $arg:expr)* $(,)?) => {
        $files.produced_by(stringify!($module), |f| $module::$function(f $(, $arg)*))
    };
}

impl OsFiles {
    /// Run `add`, a producer's code, with every file it adds owned by
    /// `producer`.
    pub fn produced_by<R>(
        &mut self,
        producer: &'static str,
        add: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let outer = self.owners.producer(producer);
        let result = add(self);
        self.owners.producer(outer);
        result
    }

    pub fn owners(&self) -> &Owners {
        &self.owners
    }

    /// Write the ownership table to `<root>/target/os-owners.tsv` and report collisions: as cargo
    /// warnings, or as an error with `LAZYOS_OWNER_STRICT=1`.
    pub fn write_owners(&self, root: &Path) -> Result<(), String> {
        println!("cargo:rerun-if-env-changed=LAZYOS_OWNER_STRICT");
        let strict =
            std::env::var_os("LAZYOS_OWNER_STRICT").as_deref() == Some(std::ffi::OsStr::new("1"));
        self.write_owners_with(root, strict)
    }

    /// [`OsFiles::write_owners`] with `strict` given rather than read from the
    /// environment.
    pub fn write_owners_with(&self, root: &Path, strict: bool) -> Result<(), String> {
        let table = root.join("target/os-owners.tsv");
        std::fs::write(&table, self.owners.table())
            .map_err(|e| format!("{}: {e}", table.display()))?;
        let collisions = self.owners.describe_collisions();
        for line in &collisions {
            println!("cargo:warning=file owner collision: {line}");
        }
        match (strict, collisions.is_empty()) {
            (true, false) => Err(format!(
                "{} path(s) have more than one owner",
                collisions.len()
            )),
            _ => Ok(()),
        }
    }
}
