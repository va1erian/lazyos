//! Store-directory selection: the first usable persistent location wins.
//!
//! Kept free of filesystem access so the ordering rule is host-testable; the
//! service supplies the "can this directory be created and written?" probe.

/// Persistent store directories, in order of preference: `/conf` on the OS
/// volume (0700 root; only `confd` reads the raw store). `/system` is written
/// only by image updates, so it is never a store.
pub const PERSISTENT_DIRS: [&str; 1] = [fhs::state::CONF_ROOT];

/// The preferred store directory: once the service is bound to it, nothing
/// better can appear.
pub const PREFERRED_DIR: &str = PERSISTENT_DIRS[0];

/// ramfs fallback used when no persistent location is writable (a recovery
/// boot with a read-only `/`): settings last until the next boot.
pub const FALLBACK_DIR: &str = fhs::state::CONF_FALLBACK;

/// Every store location, best first (the ramfs fallback last).
pub const ALL_DIRS: [&str; 2] = [PERSISTENT_DIRS[0], FALLBACK_DIR];

/// The F0 to F3 store on `/data`, seeded into [`PREFERRED_DIR`] once.
pub const LEGACY_SEED: &str = fhs::state::LEGACY_DATA_CONFD;

/// The name, inside [`PREFERRED_DIR`], of the marker written once
/// [`LEGACY_SEED`] has been merged (or found absent), so it is never read
/// again: a setting deleted after the migration stays deleted
/// (`fhs::state::CONF_SEEDED_MARKER`).
pub const SEEDED_MARKER_FILE: &str = ".seeded-from-data";

/// The locations ranked below `chosen`, where settings written while a better
/// store was unavailable may still sit (an earlier run on the ramfs), and so
/// what to merge into `chosen`, never overwriting what it holds.
pub fn seed_sources(chosen: &str) -> &'static [&'static str] {
    match ALL_DIRS.iter().position(|&dir| dir == chosen) {
        Some(index) => &ALL_DIRS[index + 1..],
        None => &[],
    }
}

/// The legacy store to seed `chosen` from once ([`crate::Confd::seed_once`]):
/// [`LEGACY_SEED`], and only into [`PREFERRED_DIR`]. Never into the ramfs,
/// where the marker could not outlive the boot.
pub fn legacy_seed_for(chosen: &str) -> Option<&'static str> {
    (chosen == PREFERRED_DIR).then_some(LEGACY_SEED)
}

/// The chosen directory and whether it survives a reboot.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Choice {
    pub dir: &'static str,
    pub persistent: bool,
}

/// Pick the first of `candidates` for which `usable` holds, else
/// [`FALLBACK_DIR`] (not persistent).
pub fn choose(candidates: &[&'static str], mut usable: impl FnMut(&str) -> bool) -> Choice {
    for &dir in candidates {
        if usable(dir) {
            return Choice {
                dir,
                persistent: true,
            };
        }
    }
    Choice {
        dir: FALLBACK_DIR,
        persistent: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conf_is_chosen_when_writable() {
        let c = choose(&PERSISTENT_DIRS, |_| true);
        assert_eq!(
            c,
            Choice {
                dir: "/conf",
                persistent: true
            }
        );
    }

    #[test]
    fn falls_back_to_transient_when_conf_is_not_writable() {
        let c = choose(&PERSISTENT_DIRS, |_| false);
        assert_eq!(
            c,
            Choice {
                dir: "/transient/conf",
                persistent: false
            }
        );
    }

    #[test]
    fn recovers_to_conf_once_it_becomes_writable() {
        // A recovery boot: `/` read-only, then remounted read/write. The
        // service re-probes the preferred directory and moves there.
        let mut writable = false;
        assert!(!choose(&PERSISTENT_DIRS, |_| writable).persistent);
        writable = true;
        let c = choose(&PERSISTENT_DIRS, |_| writable);
        assert_eq!(c.dir, PREFERRED_DIR);
        assert!(c.persistent);
        // Moving onto `/conf` merges the ramfs store and, once, `/data/confd`.
        assert_eq!(seed_sources(PREFERRED_DIR), ["/transient/conf"]);
        assert_eq!(legacy_seed_for(PREFERRED_DIR), Some("/data/confd"));
    }

    #[test]
    fn system_is_never_a_store() {
        assert!(ALL_DIRS.iter().all(|dir| !dir.starts_with(fhs::SYSTEM)));
        assert!(ALL_DIRS
            .iter()
            .all(|dir| !dir.starts_with(fhs::mount::DATA)));
    }

    #[test]
    fn only_conf_is_seeded_from_the_legacy_store() {
        assert_eq!(legacy_seed_for("/conf"), Some(LEGACY_SEED));
        assert_eq!(legacy_seed_for("/transient/conf"), None);
        assert_eq!(legacy_seed_for("/elsewhere"), None);
        assert!(seed_sources("/transient/conf").is_empty());
        assert!(seed_sources("/elsewhere").is_empty());
        let marker = alloc::format!("{PREFERRED_DIR}/{SEEDED_MARKER_FILE}");
        assert_eq!(marker, fhs::state::CONF_SEEDED_MARKER);
    }

    #[test]
    fn stops_probing_after_first_success() {
        let mut probed = 0;
        choose(&PERSISTENT_DIRS, |_| {
            probed += 1;
            true
        });
        assert_eq!(probed, 1);
    }
}
