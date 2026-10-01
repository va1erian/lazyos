//! Store-directory selection: the first usable persistent location wins.
//!
//! Kept free of filesystem access so the ordering rule is host-testable; the
//! service supplies the "can this directory be created and written?" probe.

/// Persistent store directories, in order of preference.
///
/// `/data/confd` (the writable ext2 data volume, present when a data disk is
/// attached) is preferred: on the shipped image `/system` is the read-only FAT
/// boot volume, so `/system/confd` cannot be created there and is kept only as
/// the planned home for when a writable system volume exists.
pub const PERSISTENT_DIRS: [&str; 2] = [fhs::state::CONFD_DIRS[0], fhs::state::CONFD_DIRS[1]];

/// The preferred store directory: once the service is bound to it, nothing
/// better can appear.
pub const PREFERRED_DIR: &str = PERSISTENT_DIRS[0];

/// Every store location, best first (the ramfs fallback last).
pub const ALL_DIRS: [&str; 3] = [PERSISTENT_DIRS[0], PERSISTENT_DIRS[1], FALLBACK_DIR];

/// The locations ranked below `chosen`: where settings written while a better
/// store was unavailable may still sit, and so what to seed `chosen` from.
pub fn seed_sources(chosen: &str) -> &'static [&'static str] {
    match ALL_DIRS.iter().position(|&dir| dir == chosen) {
        Some(index) => &ALL_DIRS[index + 1..],
        None => &[],
    }
}

/// ramfs fallback used when no persistent location is writable.
pub const FALLBACK_DIR: &str = fhs::state::CONFD_DIRS[2];

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
    fn prefers_data_over_system() {
        let c = choose(&PERSISTENT_DIRS, |_| true);
        assert_eq!(
            c,
            Choice {
                dir: "/data/confd",
                persistent: true
            }
        );
    }

    #[test]
    fn falls_through_to_system_when_data_unusable() {
        let c = choose(&PERSISTENT_DIRS, |d| d == "/system/confd");
        assert_eq!(
            c,
            Choice {
                dir: "/system/confd",
                persistent: true
            }
        );
    }

    #[test]
    fn falls_back_to_ramfs_when_nothing_writable() {
        let c = choose(&PERSISTENT_DIRS, |_| false);
        assert_eq!(
            c,
            Choice {
                dir: FALLBACK_DIR,
                persistent: false
            }
        );
    }

    #[test]
    fn seed_sources_are_the_lower_ranked_dirs() {
        assert_eq!(seed_sources("/data/confd"), ["/system/confd", "/tmp/confd"]);
        assert_eq!(seed_sources("/system/confd"), ["/tmp/confd"]);
        assert!(seed_sources("/tmp/confd").is_empty());
        assert!(seed_sources("/elsewhere").is_empty());
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
