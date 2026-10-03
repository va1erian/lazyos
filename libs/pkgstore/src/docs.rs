//! Where an installed app's documentation goes, and the plan that keeps it
//! whole across an upgrade.
//!
//! On install `pkgd` copies the package's `docs/**.md` to
//! `/docs/apps/<system_name>/`, so the Docs app (rooted at `/docs`) lists it
//! next to the OS documentation. The copy is written to `<system_name>~new`
//! first; only when it is complete does it replace the live directory
//! (`<system_name>` → `<system_name>~old`, `~new` → `<system_name>`, then the
//! `~old` tree is deleted), so the docs never describe a half-upgrade (`~`,
//! because `.new` is a valid `system_name` label and would name another app).
//! A stop between those steps is repaired at startup by [`recovery`].
//!
//! Every path is composed here from a validated `system_name` and confined by
//! [`deletable`] to strictly below `/docs/apps`, as `pkgd`'s removals of an
//! install directory are confined to `/apps`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::access::{under, well_formed};
use crate::layout::{
    safe_entry, valid_system_name, PathError, DOCS_DIR, DOCS_RETIRED, DOCS_ROOT, DOCS_STAGING,
};

/// `/docs/apps/<system_name>`: the live documentation of one app.
pub fn docs_dir(system_name: &str) -> Result<String, PathError> {
    named(system_name, "")
}

/// `/docs/apps/<system_name>~new`: the copy being written.
pub fn staging_dir(system_name: &str) -> Result<String, PathError> {
    named(system_name, DOCS_STAGING)
}

/// `/docs/apps/<system_name>~old`: the previous copy while it is replaced.
pub fn retired_dir(system_name: &str) -> Result<String, PathError> {
    named(system_name, DOCS_RETIRED)
}

fn named(system_name: &str, suffix: &str) -> Result<String, PathError> {
    if !valid_system_name(system_name) {
        return Err(PathError::SystemName);
    }
    Ok(format!("{DOCS_ROOT}/{system_name}{suffix}"))
}

/// The documentation file of a package entry, relative to the app's docs
/// directory: `docs/guide/intro.md` is `guide/intro.md`. `None` for anything
/// that is not a `.md` file under the package's `docs/` (a directory entry,
/// another tree), and `Err` for an unsafe name.
pub fn doc_file(entry: &str, is_dir: bool) -> Result<Option<&str>, PathError> {
    if !safe_entry(entry) {
        return Err(PathError::Entry);
    }
    let Some(rest) = entry
        .strip_prefix(DOCS_DIR)
        .and_then(|r| r.strip_prefix('/'))
    else {
        return Ok(None);
    };
    Ok((!is_dir && !rest.is_empty() && rest.ends_with(".md")).then_some(rest))
}

/// The top-level entry name under `/docs/apps` that `name` stands for: the
/// app's `system_name` for `<sn>`, `<sn>~new` and `<sn>~old`, else `None`.
fn owner_of(name: &str) -> Option<&str> {
    let base = name
        .strip_suffix(DOCS_STAGING)
        .or_else(|| name.strip_suffix(DOCS_RETIRED))
        .unwrap_or(name);
    valid_system_name(base).then_some(base)
}

/// Whether `pkgd` may delete `path`: well formed, strictly below
/// `/docs/apps`, inside a directory named for a valid `system_name` (or its
/// `~new`/`~old` copy). Nothing else under `/docs` is ever touched.
pub fn deletable(path: &str) -> bool {
    if !well_formed(path) || !under(path, DOCS_ROOT) {
        return false;
    }
    let rest = &path[DOCS_ROOT.len() + 1..];
    let top = rest.split('/').next().unwrap_or("");
    owner_of(top).is_some()
}

/// One repair step for what a stop mid-replacement left in `/docs/apps`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Repair {
    /// Delete an unfinished or superseded copy.
    Remove(String),
    /// Make a complete copy the live one.
    Rename { from: String, to: String },
}

/// The repairs for a listing of `/docs/apps` (`names` are entry names), per
/// app, from the order the replacement runs in (write `~new` completely,
/// live → `~old`, `~new` → live, delete `~old`):
///
/// * a live directory is current: any `~new` or `~old` beside it goes;
/// * no live one but a `~old`: the stop came between the renames, so a `~new`
///   is complete and becomes live (else the `~old` comes back), and the
///   `~old` goes;
/// * only a `~new`: it may be partial, so it goes.
///
/// Names that are not `pkgd`'s are left alone.
pub fn recovery(names: &[&str]) -> Vec<Repair> {
    let mut repairs = Vec::new();
    let mut owners: Vec<&str> = names.iter().filter_map(|name| owner_of(name)).collect();
    owners.sort_unstable();
    owners.dedup();
    for owner in owners {
        let has = |suffix: &str| {
            names
                .iter()
                .any(|name| name.strip_suffix(suffix) == Some(owner))
        };
        let path = |suffix: &str| format!("{DOCS_ROOT}/{owner}{suffix}");
        let (live, staged, retired) =
            (names.contains(&owner), has(DOCS_STAGING), has(DOCS_RETIRED));
        if !live && retired {
            let from = if staged { DOCS_STAGING } else { DOCS_RETIRED };
            repairs.push(Repair::Rename {
                from: path(from),
                to: path(""),
            });
            if staged {
                repairs.push(Repair::Remove(path(DOCS_RETIRED)));
            }
            continue;
        }
        if staged {
            repairs.push(Repair::Remove(path(DOCS_STAGING)));
        }
        if retired {
            repairs.push(Repair::Remove(path(DOCS_RETIRED)));
        }
    }
    repairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_built_from_valid_names_only() {
        assert_eq!(
            docs_dir("org.lazy.counter").unwrap(),
            "/docs/apps/org.lazy.counter"
        );
        assert_eq!(
            staging_dir("org.lazy.counter").unwrap(),
            "/docs/apps/org.lazy.counter~new"
        );
        assert_eq!(
            retired_dir("org.lazy.counter").unwrap(),
            "/docs/apps/org.lazy.counter~old"
        );
        for bad in ["", "..", "../os", "org.lazy", "a/b.c.d", "Org.Lazy.X"] {
            assert_eq!(docs_dir(bad), Err(PathError::SystemName), "{bad}");
            assert_eq!(staging_dir(bad), Err(PathError::SystemName), "{bad}");
        }
    }

    #[test]
    fn only_markdown_under_docs_is_published() {
        assert_eq!(doc_file("docs/README.md", false), Ok(Some("README.md")));
        assert_eq!(
            doc_file("docs/guide/intro.md", false),
            Ok(Some("guide/intro.md"))
        );
        assert_eq!(doc_file("docs/", true), Ok(None));
        assert_eq!(doc_file("docs/guide/", true), Ok(None));
        assert_eq!(doc_file("docs/notes.txt", false), Ok(None));
        assert_eq!(doc_file("docsx/a.md", false), Ok(None));
        assert_eq!(doc_file("resources/docs/a.md", false), Ok(None));
        assert_eq!(doc_file("bin/app.elf", false), Ok(None));
        assert_eq!(doc_file("docs/../x.md", false), Err(PathError::Entry));
        assert_eq!(doc_file("/docs/x.md", false), Err(PathError::Entry));
    }

    /// Removals are confined to `pkgd`'s own directories below `/docs/apps`.
    #[test]
    fn deletion_is_confined_to_app_docs() {
        for ok in [
            "/docs/apps/org.lazy.counter",
            "/docs/apps/org.lazy.counter/README.md",
            "/docs/apps/org.lazy.counter~new",
            "/docs/apps/org.lazy.counter~new/guide/a.md",
            "/docs/apps/org.lazy.counter~old",
        ] {
            assert!(deletable(ok), "{ok}");
        }
        for bad in [
            "/docs",
            "/docs/apps",
            "/docs/apps/",
            "/docs/os",
            "/docs/os/README.md",
            "/docs/apps/../os",
            "/docs/apps/org.lazy.counter/../../os",
            "/docs/apps/notes.md",
            "/docs/apps/org.lazy",
            "/docs/apps/org.lazy.counter~tmp",
            "/docs/appsx/org.lazy.counter",
            "/apps/org.lazy.counter",
            "/docs/apps//org.lazy.counter",
            "docs/apps/org.lazy.counter",
        ] {
            assert!(!deletable(bad), "{bad}");
        }
    }

    #[test]
    fn recovery_finishes_or_undoes_an_interrupted_replacement() {
        let remove = |p: &str| Repair::Remove(String::from(p));
        let rename = |from: &str, to: &str| Repair::Rename {
            from: String::from(from),
            to: String::from(to),
        };
        // Stopped while writing the copy: the live docs stay, the copy goes.
        assert_eq!(
            recovery(&["org.lazy.a", "org.lazy.a~new"]),
            [remove("/docs/apps/org.lazy.a~new")]
        );
        // Stopped between the renames: the complete new copy becomes live.
        assert_eq!(
            recovery(&["org.lazy.a~old", "org.lazy.a~new"]),
            [
                rename("/docs/apps/org.lazy.a~new", "/docs/apps/org.lazy.a"),
                remove("/docs/apps/org.lazy.a~old"),
            ]
        );
        // Only the old copy left: it comes back.
        assert_eq!(
            recovery(&["org.lazy.a~old"]),
            [rename("/docs/apps/org.lazy.a~old", "/docs/apps/org.lazy.a")]
        );
        // Stopped before deleting the old copy: it goes.
        assert_eq!(
            recovery(&["org.lazy.a", "org.lazy.a~old"]),
            [remove("/docs/apps/org.lazy.a~old")]
        );
        // A first install stopped mid-copy: the partial copy goes.
        assert_eq!(
            recovery(&["org.lazy.b~new"]),
            [remove("/docs/apps/org.lazy.b~new")]
        );
        // Nothing to do, and names that are not pkgd's are left alone.
        assert!(recovery(&["org.lazy.a", "readme.md", "x~new", "..", "org.lazy.b"]).is_empty());
        // An app whose name ends in `.new` is an app, not a copy.
        assert!(recovery(&["org.lazy.a", "org.lazy.a.new", "org.lazy.a.old"]).is_empty());
    }
}
