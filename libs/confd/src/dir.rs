//! Store-directory selection: the first usable persistent location wins.
//!
//! Kept free of filesystem access so the ordering rule is host-testable; the
//! service supplies the "can this directory be created and written?" probe.

/// Persistent store directories, in order of preference.
///
/// `/system/confd` is the planned home; `/data/confd` is the ext2 data volume
/// that exists when a data disk is attached.
pub const PERSISTENT_DIRS: [&str; 2] = ["/system/confd", "/data/confd"];

/// ramfs fallback used when no persistent location is writable.
pub const FALLBACK_DIR: &str = "/tmp/confd";

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
    fn prefers_system_over_data() {
        let c = choose(&PERSISTENT_DIRS, |_| true);
        assert_eq!(
            c,
            Choice {
                dir: "/system/confd",
                persistent: true
            }
        );
    }

    #[test]
    fn falls_through_to_data_volume() {
        let c = choose(&PERSISTENT_DIRS, |d| d == "/data/confd");
        assert_eq!(
            c,
            Choice {
                dir: "/data/confd",
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
    fn stops_probing_after_first_success() {
        let mut probed = 0;
        choose(&PERSISTENT_DIRS, |_| {
            probed += 1;
            true
        });
        assert_eq!(probed, 1);
    }
}
